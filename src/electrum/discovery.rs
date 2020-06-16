use std::cmp::Ordering;
use std::collections::{hash_map::Entry, BinaryHeap, HashMap, HashSet};
use std::fmt;
use std::net::{IpAddr, SocketAddr, ToSocketAddrs};
use std::str::FromStr;
use std::sync::{Arc, RwLock};
use std::thread;
use std::time::{Duration, Instant};

use bitcoin::BlockHash;

use crate::chain::Network;
use crate::electrum::{Client, Hostname, Port, ProtocolVersion, ServerFeatures};
use crate::errors::{Result, ResultExt};
use crate::util::spawn_thread;

mod default_servers;
use default_servers::add_default_servers;

const HEALTH_CHECK_FREQ: Duration = Duration::from_secs(3600); // check servers every hour
const JOB_INTERVAL: Duration = Duration::from_secs(1); // run one health check job every second
const MAX_CONSECUTIVE_FAILURES: usize = 24; // drop servers after 24 consecutive failing attempts (~24 hours) (~24 hours)
const MAX_QUEUE_SIZE: usize = 500; // refuse accepting new servers if we have that many health check jobs
const MAX_SERVICES_PER_REQUEST: usize = 5; // maximum number of services added per server.add_peer call

#[derive(Default, Debug)]
pub struct DiscoveryManager {
    /// A queue of scheduled health check jobs, including for healthy, unhealthy and untested servers
    queue: RwLock<BinaryHeap<HealthCheck>>,

    /// A list of servers that were found to be healthy on their last health check
    healthy: RwLock<HashMap<ServerAddr, Server>>,

    /// A cache of hostname dns resolutions
    hostnames: RwLock<HashMap<Hostname, IpAddr>>,

    /// Used to test for compatibility
    our_genesis_hash: BlockHash,
    our_version: ProtocolVersion,

    /// Optional, will not support onion hosts without this
    tor_proxy: Option<SocketAddr>,
}

/// A Server corresponds to a single IP address or onion hostname, with one or more services
/// exposed on different ports.
#[derive(Debug)]
struct Server {
    services: HashSet<Service>,
    hostname: Hostname,
    features: ServerFeatures,
    // the `ServerAddr` isn't kept here directly, but is also available next to `Server` as the key for
    // the `healthy` field on `DiscoveryManager`
}

#[derive(Eq, PartialEq, Hash, Clone, Debug)]
enum ServerAddr {
    Clearnet(IpAddr),
    Onion(Hostname),
}

#[derive(Eq, PartialEq, Hash, Copy, Clone, Debug)]
pub enum Service {
    Tcp(Port),
    Ssl(Port),
    // unimplemented: Ws and Wss
}

/// A queued health check job, one per service/port (and not per server)
#[derive(Eq, Debug)]
struct HealthCheck {
    hostname: Hostname,
    addr: Option<ServerAddr>,
    service: Service,
    is_default: bool,
    added_by: Option<IpAddr>,
    last_check: Option<Instant>,
    last_healthy: Option<Instant>,
    consecutive_failures: usize,
}

/// The server entry format returned from server.peers.subscribe
#[derive(Serialize)]
pub struct ServerEntry(ServerAddr, Hostname, Vec<String>);

impl DiscoveryManager {
    pub fn new(
        our_network: Network,
        our_version: ProtocolVersion,
        tor_proxy: Option<SocketAddr>,
    ) -> Self {
        let discovery = Self {
            our_genesis_hash: our_network.genesis_hash(),
            our_version,
            tor_proxy,
            ..Default::default()
        };
        add_default_servers(&discovery, our_network);
        discovery
    }

    /// Add a server requested via `server.add_peer`
    pub fn add_server_request(&self, added_by: IpAddr, features: ServerFeatures) -> Result<()> {
        let mut queue = self.queue.write().unwrap();

        ensure!(queue.len() < MAX_QUEUE_SIZE, "queue size exceeded");

        self.verify_compatibility(&features)?;

        // collect HealthChecks for candidate services
        let jobs = features
            .hosts
            .iter()
            .filter(|(hostname, _)| {
                if hostname.len() > 100 {
                    warn!("skipping invalid hostname");
                    false
                } else {
                    true
                }
            })
            .flat_map(|(hostname, ports)| {
                let hostname = hostname.to_lowercase();
                let tcp_service = ports.tcp_port.into_iter().map(Service::Tcp);
                let ssl_service = ports.ssl_port.into_iter().map(Service::Ssl);
                let services = tcp_service.chain(ssl_service).collect::<HashSet<Service>>();
                // XXX reject invalid source ip here?

                // find new services, skip ones that are we already know about
                let services =
                    match ServerAddr::resolve_noio(&hostname, &self.hostnames.read().unwrap()) {
                        Some(addr) => match self.healthy.read().unwrap().get(&addr) {
                            Some(server) => {
                                services.difference(&server.services).cloned().collect()
                            }
                            None => services,
                        },
                        None => services,
                    };

                services
                    .into_iter()
                    .map(move |service| HealthCheck::new(hostname.clone(), service, Some(added_by)))
            })
            .take(MAX_SERVICES_PER_REQUEST)
            .collect::<Vec<HealthCheck>>();

        ensure!(
            !jobs.is_empty(),
            "no valid services, or all services are known already"
        );

        ensure!(
            queue.len() + jobs.len() <= MAX_QUEUE_SIZE,
            "queue size exceeded"
        );

        queue.extend(jobs);
        Ok(())
    }

    /// Add a default server. Default servers are exempt from limits and given more leniency
    /// before being removed due to unavailability.
    pub fn add_default_server(&self, hostname: Hostname, services: Vec<Service>) {
        let mut queue = self.queue.write().unwrap();
        queue.extend(
            services
                .into_iter()
                .map(|service| HealthCheck::new(hostname.clone(), service, None)),
        );
    }

    /// Get the list of healthy servers formatted for `servers.peers.subscribe`
    pub fn get_servers(&self) -> Vec<ServerEntry> {
        // XXX return a random sample instead of everything?
        self.healthy
            .read()
            .unwrap()
            .iter()
            .map(|(addr, server)| {
                ServerEntry(addr.clone(), server.hostname.clone(), server.feature_strs())
            })
            .collect()
    }

    /// Run the next health check in the queue (a single one)
    fn run_health_check(&self) -> Result<()> {
        // abort if there are no entries in the queue, or its still too early for the next one up
        if self.queue.read().unwrap().peek().map_or(true, |next| {
            next.last_check
                .map_or(false, |t| t.elapsed() < HEALTH_CHECK_FREQ)
        }) {
            return Ok(());
        }

        let mut health_check = self.queue.write().unwrap().pop().unwrap();
        debug!("processing {:?}", health_check);

        let resolve_and_check = |health_check: &mut HealthCheck| {
            // this is run once for each new health check job (they are always initialized without an addr)
            if health_check.addr.is_none() {
                let addr = ServerAddr::resolve(
                    &health_check.hostname,
                    &mut self.hostnames.write().unwrap(),
                )
                .chain_err(|| "hostname does not resolve")?;

                if let Some(server) = self.healthy.read().unwrap().get(&addr) {
                    ensure!(
                        !server.services.contains(&health_check.service),
                        "dropping duplicated service"
                    )
                }

                // ensure the server address matches the ip that advertised it to us. onion hosts are exempt.
                if let (ServerAddr::Clearnet(server_ip), Some(added_by)) =
                    (&addr, &health_check.added_by)
                {
                    ensure!(server_ip == added_by, "server ip does not match source ip");
                }

                health_check.addr = Some(addr.clone());
            };

            let addr = health_check.addr.as_ref().unwrap();
            self.check_server(addr, &health_check.hostname, health_check.service)
        };

        let was_healthy = health_check.is_healthy();

        match resolve_and_check(&mut health_check) {
            Ok(features) => {
                debug!(
                    "{} {:?} is available",
                    health_check.hostname, health_check.service
                );

                if !was_healthy {
                    self.save_healthy_service(&health_check, features);
                }
                // XXX update features?

                health_check.last_check = Some(Instant::now());
                health_check.last_healthy = health_check.last_check;
                health_check.consecutive_failures = 0;
                // schedule the next health check
                self.queue.write().unwrap().push(health_check);

                Ok(())
            }
            Err(e) => {
                debug!(
                    "{} {:?} is unavailable: {:?}",
                    health_check.hostname, health_check.service, e
                );

                if was_healthy {
                    // XXX should we assume the server's other services are down too?
                    self.remove_unhealthy_service(&health_check);
                }

                health_check.last_check = Some(Instant::now());
                health_check.consecutive_failures += 1;

                if health_check.should_retry() {
                    self.queue.write().unwrap().push(health_check);
                } else {
                    debug!("giving up on {:?}", health_check);
                }

                Err(e)
            }
        }
    }

    /// Upsert the server/service into the healthy set
    fn save_healthy_service(&self, health_check: &HealthCheck, features: ServerFeatures) {
        let addr = health_check.addr.clone().unwrap();
        let mut healthy = self.healthy.write().unwrap();
        assert!(healthy
            .entry(addr)
            .or_insert_with(|| Server::new(health_check.hostname.clone(), features))
            .services
            .insert(health_check.service));
    }

    /// Remove the service, and remove the server entirely if it has no other reamining healthy services
    fn remove_unhealthy_service(&self, health_check: &HealthCheck) {
        let addr = health_check.addr.clone().unwrap();
        let mut healthy = self.healthy.write().unwrap();
        if let Entry::Occupied(mut entry) = healthy.entry(addr) {
            let server = entry.get_mut();
            assert!(server.services.remove(&health_check.service));
            if server.services.is_empty() {
                entry.remove_entry();
                // TODO evict hostname entries for servers that never worked
                self.hostnames
                    .write()
                    .unwrap()
                    .remove(&health_check.hostname);
            }
        } else {
            panic!("missing expected server");
        }
    }

    fn check_server(
        &self,
        addr: &ServerAddr,
        hostname: &Hostname,
        service: Service,
    ) -> Result<ServerFeatures> {
        debug!("checking service {:?} {:?}", addr, service);

        let mut client: Client = match (addr, service) {
            (ServerAddr::Clearnet(ip), Service::Tcp(port)) => Client::new((*ip, port))?,
            (ServerAddr::Clearnet(_), Service::Ssl(port)) => Client::new_ssl((hostname, port))?,
            (ServerAddr::Onion(hostname), Service::Tcp(port)) => {
                let tor_proxy = self
                    .tor_proxy
                    .chain_err(|| "no tor proxy configured, onion hosts are unsupported")?;
                Client::new_proxy((hostname, port), tor_proxy)?
            }
            (ServerAddr::Onion(_), Service::Ssl(_)) => bail!("ssl over onion is unsupported"),
        };

        let features = client.server_features()?;
        self.verify_compatibility(&features)?;

        // TODO register ourselves with add_peer
        // XXX should we require this to succeed?
        //ensure!(client.add_peer(self.our_features)?, "server does not reciprocate");

        Ok(features)
    }

    fn verify_compatibility(&self, features: &ServerFeatures) -> Result<()> {
        ensure!(
            features.genesis_hash == self.our_genesis_hash,
            "incompatible networks"
        );

        ensure!(
            features.protocol_min <= self.our_version && features.protocol_max >= self.our_version,
            "incompatible protocol versions"
        );

        ensure!(
            features.hash_function == "sha256",
            "incompatible hash function"
        );

        Ok(())
    }

    pub fn spawn_jobs_thread(manager: Arc<DiscoveryManager>) {
        spawn_thread("discovery-jobs", move || loop {
            if let Err(e) = manager.run_health_check() {
                debug!("health check failed: {:?}", e);
            }
            // XXX use a dynamic JOB_INTERVAL, adjusted according to the queue size and HEALTH_CHECK_FREQ?
            thread::sleep(JOB_INTERVAL);
        });
    }
}

impl Server {
    fn new(hostname: Hostname, features: ServerFeatures) -> Self {
        Server {
            hostname,
            features,
            services: HashSet::new(),
        }
    }

    /// Get server features and services in the compact string array format used for `servers.peers.subscribe`
    fn feature_strs(&self) -> Vec<String> {
        let mut strs = Vec::with_capacity(self.services.len() + 1);
        strs.push(format!("v{}", self.features.protocol_max));
        if let Some(pruning) = self.features.pruning {
            strs.push(format!("p{}", pruning));
        }
        strs.extend(self.services.iter().map(|s| s.to_string()));
        strs
    }
}

impl ServerAddr {
    /// Attempt resolving the hostname without issuing a DNS query
    fn resolve_noio(host: &str, cache: &HashMap<Hostname, IpAddr>) -> Option<Self> {
        if host.ends_with(".onion") {
            Some(ServerAddr::Onion(host.into()))
        } else if let Ok(ip) = IpAddr::from_str(host) {
            Some(ServerAddr::Clearnet(ip))
        } else if let Some(ip) = cache.get(host) {
            Some(ServerAddr::Clearnet(*ip))
        } else {
            None
        }
    }

    /// Attempt resolving the hostname, with a DNS query if necessary
    fn resolve(host: &str, cache: &mut HashMap<Hostname, IpAddr>) -> Result<Self> {
        if let Some(addr) = ServerAddr::resolve_noio(host, cache) {
            Ok(addr)
        } else {
            let ip = format!("{}:1", host)
                .to_socket_addrs()
                .chain_err(|| "hostname resolution failed")?
                .next()
                .chain_err(|| "hostname resolution failed")?
                .ip();
            cache.insert(host.into(), ip);
            Ok(ServerAddr::Clearnet(ip))
        }
    }
}

impl fmt::Display for ServerAddr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ServerAddr::Clearnet(ip) => write!(f, "{}", ip),
            ServerAddr::Onion(hostname) => write!(f, "{}", hostname),
        }
    }
}

impl serde::Serialize for ServerAddr {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(&self.to_string())
    }
}

impl HealthCheck {
    fn new(hostname: Hostname, service: Service, added_by: Option<IpAddr>) -> Self {
        HealthCheck {
            hostname,
            service,
            is_default: added_by.is_none(),
            added_by,
            addr: None,
            last_check: None,
            last_healthy: None,
            consecutive_failures: 0,
        }
    }

    fn is_healthy(&self) -> bool {
        match (self.last_check, self.last_healthy) {
            (Some(last_check), Some(last_healthy)) => last_check == last_healthy,
            _ => false,
        }
    }

    // allow the server to fail up to MAX_CONSECTIVE_FAILURES time before giving up on it.
    // if its a non-default server and the very first attempt fails, give up immediatly.
    fn should_retry(&self) -> bool {
        (self.last_healthy.is_some() || self.is_default)
            && self.consecutive_failures < MAX_CONSECUTIVE_FAILURES
    }
}

impl PartialEq for HealthCheck {
    fn eq(&self, other: &Self) -> bool {
        self.hostname == other.hostname && self.service == other.service
    }
}

impl Ord for HealthCheck {
    fn cmp(&self, other: &Self) -> Ordering {
        self.last_check.cmp(&other.last_check).reverse()
    }
}

impl PartialOrd for HealthCheck {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl fmt::Display for Service {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Service::Tcp(port) => write!(f, "t{}", port),
            Service::Ssl(port) => write!(f, "s{}", port),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chain::Network;
    use std::time;

    #[test]
    fn test() -> Result<()> {
        stderrlog::new().verbosity(4).init().unwrap();

        let discovery = DiscoveryManager::new(
            Network::Testnet,
            "1.4".parse().unwrap(),
            Some("127.0.0.1:9150".parse().unwrap()),
        );

        discovery.add_default_server(
            "electrum.blockstream.info".into(),
            vec![Service::Tcp(60001)],
        );
        discovery.add_default_server("testnet.hsmiths.com".into(), vec![Service::Ssl(53012)]);
        discovery.add_default_server(
            "tn.not.fyi".into(),
            vec![Service::Tcp(55001), Service::Ssl(55002)],
        );
        discovery.add_default_server(
            "electrum.blockstream.info".into(),
            vec![Service::Tcp(60001), Service::Ssl(60002)],
        );
        discovery.add_default_server(
            "explorerzydxu5ecjrkwceayqybizmpjjznk5izmitf2modhcusuqlid.onion".into(),
            vec![Service::Tcp(143)],
        );

        debug!("{:#?}", discovery);

        for _ in 0..12 {
            discovery
                .run_health_check()
                .map_err(|e| warn!("{:?}", e))
                .ok();
            thread::sleep(time::Duration::from_secs(1));
        }

        debug!("{:#?}", discovery);

        info!("{}", json!(discovery.get_servers()));

        Ok(())
    }
}
