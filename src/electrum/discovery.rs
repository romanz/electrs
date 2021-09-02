use std::cmp::Ordering;
use std::collections::{hash_map::Entry, BinaryHeap, HashMap, HashSet};
use std::convert::TryInto;
use std::fmt;
use std::net::{IpAddr, SocketAddr, ToSocketAddrs};
use std::str::FromStr;
use std::sync::{Arc, RwLock};
use std::thread;
use std::time::{Duration, Instant};

use electrum_client::ElectrumApi;

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
const MAX_SERVERS_PER_REQUEST: usize = 3; // maximum number of server hosts added per server.add_peer call
const MAX_SERVICES_PER_REQUEST: usize = 6; // maximum number of services added per server.add_peer call

#[derive(Debug)]
pub struct DiscoveryManager {
    /// A queue of scheduled health check jobs, including for healthy, unhealthy and untested servers
    queue: RwLock<BinaryHeap<HealthCheck>>,

    /// A list of servers that were found to be healthy on their last health check
    healthy: RwLock<HashMap<ServerAddr, Server>>,

    /// Used to test for protocol version compatibility
    our_version: ProtocolVersion,

    /// So that we don't list ourselves
    our_addrs: HashSet<ServerAddr>,

    /// For advertising ourself to other servers
    our_features: ServerFeatures,

    /// Whether we should announce ourselves to the servers we're connecting to
    announce: bool,

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
    addr: ServerAddr,
    hostname: Hostname,
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
        our_features: ServerFeatures,
        our_version: ProtocolVersion,
        announce: bool,
        tor_proxy: Option<SocketAddr>,
    ) -> Self {
        let our_addrs = our_features
            .hosts
            .keys()
            .filter_map(|hostname| {
                ServerAddr::resolve(hostname)
                    .map_err(|e| warn!("failed resolving own hostname {}: {:?}", hostname, e))
                    .ok()
            })
            .collect();
        let discovery = Self {
            our_addrs,
            our_version,
            our_features,
            announce,
            tor_proxy,
            healthy: Default::default(),
            queue: Default::default(),
        };
        add_default_servers(&discovery, our_network);
        discovery
    }

    /// Add a server requested via `server.add_peer`
    pub fn add_server_request(&self, added_by: IpAddr, features: ServerFeatures) -> Result<()> {
        self.verify_compatibility(&features)?;

        let mut queue = self.queue.write().unwrap();
        ensure!(queue.len() < MAX_QUEUE_SIZE, "queue size exceeded");

        // TODO optimize
        let mut existing_services: HashMap<ServerAddr, HashSet<Service>> = HashMap::new();
        for job in queue.iter() {
            existing_services
                .entry(job.addr.clone())
                .or_default()
                .insert(job.service);
        }

        // collect HealthChecks for candidate services
        let jobs = features
            .hosts
            .iter()
            .take(MAX_SERVERS_PER_REQUEST)
            .filter_map(|(hostname, ports)| {
                let hostname = hostname.to_lowercase();

                if hostname.len() > 100 {
                    warn!("skipping invalid hostname");
                    return None;
                }
                let addr = match ServerAddr::resolve(&hostname) {
                    Ok(addr) => addr,
                    Err(e) => {
                        warn!("failed resolving {}: {:?}", hostname, e);
                        return None;
                    }
                };
                if !is_remote_addr(&addr) || self.our_addrs.contains(&addr) {
                    warn!("skipping own or non-remote server addr");
                    return None;
                }
                // ensure the server address matches the ip that advertised it to us.
                // onion hosts are exempt.
                if let ServerAddr::Clearnet(ip) = addr {
                    if ip != added_by {
                        warn!(
                            "server ip does not match source ip ({}, {} != {})",
                            hostname, ip, added_by
                        );
                        return None;
                    }
                }
                Some((addr, hostname, ports))
            })
            .flat_map(|(addr, hostname, ports)| {
                let tcp_service = ports.tcp_port.into_iter().map(Service::Tcp);
                let ssl_service = ports.ssl_port.into_iter().map(Service::Ssl);
                let services = tcp_service.chain(ssl_service).collect::<HashSet<Service>>();

                services
                    .into_iter()
                    .filter(|service| {
                        existing_services
                            .get(&addr)
                            .map_or(true, |s| !s.contains(service))
                    })
                    .map(|service| {
                        HealthCheck::new(addr.clone(), hostname.clone(), service, Some(added_by))
                    })
                    .collect::<Vec<_>>()
            })
            .take(MAX_SERVICES_PER_REQUEST)
            .collect::<Vec<_>>();

        ensure!(
            queue.len() + jobs.len() <= MAX_QUEUE_SIZE,
            "queue size exceeded"
        );

        queue.extend(jobs);
        Ok(())
    }

    /// Add a default server. Default servers are exempt from limits and given more leniency
    /// before being removed due to unavailability.
    pub fn add_default_server(&self, hostname: Hostname, services: Vec<Service>) -> Result<()> {
        let addr = ServerAddr::resolve(&hostname)?;
        let mut queue = self.queue.write().unwrap();
        queue.extend(
            services
                .into_iter()
                .map(|service| HealthCheck::new(addr.clone(), hostname.clone(), service, None)),
        );
        Ok(())
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

    pub fn our_features(&self) -> &ServerFeatures {
        &self.our_features
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

        let mut job = self.queue.write().unwrap().pop().unwrap();
        debug!("processing {:?}", job);

        let was_healthy = job.is_healthy();

        match self.check_server(&job.addr, &job.hostname, job.service) {
            Ok(features) => {
                debug!("{} {:?} is available", job.hostname, job.service);

                if !was_healthy {
                    self.save_healthy_service(&job, features);
                }
                // XXX update features?

                job.last_check = Some(Instant::now());
                job.last_healthy = job.last_check;
                job.consecutive_failures = 0;
                // schedule the next health check
                self.queue.write().unwrap().push(job);

                Ok(())
            }
            Err(e) => {
                debug!("{} {:?} is unavailable: {:?}", job.hostname, job.service, e);

                if was_healthy {
                    // XXX should we assume the server's other services are down too?
                    self.remove_unhealthy_service(&job);
                }

                job.last_check = Some(Instant::now());
                job.consecutive_failures += 1;

                if job.should_retry() {
                    self.queue.write().unwrap().push(job);
                } else {
                    debug!("giving up on {:?}", job);
                }

                Err(e)
            }
        }
    }

    /// Upsert the server/service into the healthy set
    fn save_healthy_service(&self, job: &HealthCheck, features: ServerFeatures) {
        let addr = job.addr.clone();
        let mut healthy = self.healthy.write().unwrap();
        healthy
            .entry(addr)
            .or_insert_with(|| Server::new(job.hostname.clone(), features))
            .services
            .insert(job.service);
    }

    /// Remove the service, and remove the server entirely if it has no other reamining healthy services
    fn remove_unhealthy_service(&self, job: &HealthCheck) {
        let addr = job.addr.clone();
        let mut healthy = self.healthy.write().unwrap();
        if let Entry::Occupied(mut entry) = healthy.entry(addr) {
            let server = entry.get_mut();
            assert!(server.services.remove(&job.service));
            if server.services.is_empty() {
                entry.remove_entry();
            }
        } else {
            unreachable!("missing expected server, corrupted state");
        }
    }

    fn check_server(
        &self,
        addr: &ServerAddr,
        hostname: &Hostname,
        service: Service,
    ) -> Result<ServerFeatures> {
        debug!("checking service {:?} {:?}", addr, service);

        let server_url = match (addr, service) {
            (ServerAddr::Clearnet(ip), Service::Tcp(port)) => format!("tcp://{}:{}", ip, port),
            (ServerAddr::Clearnet(_), Service::Ssl(port)) => format!("ssl://{}:{}", hostname, port),
            (ServerAddr::Onion(onion_host), Service::Tcp(port)) => {
                format!("tcp://{}:{}", onion_host, port)
            }
            (ServerAddr::Onion(onion_host), Service::Ssl(port)) => {
                format!("ssl://{}:{}", onion_host, port)
            }
        };

        let mut config = electrum_client::ConfigBuilder::new();
        if let ServerAddr::Onion(_) = addr {
            let socks = electrum_client::Socks5Config::new(
                self.tor_proxy
                    .chain_err(|| "no tor proxy configured, onion hosts are unsupported")?,
            );
            config = config.socks5(Some(socks)).unwrap()
        }

        let client = Client::from_config(&server_url, config.build())?;

        let features = client.server_features()?.try_into()?;
        self.verify_compatibility(&features)?;

        if self.announce {
            // XXX should we require the other side to reciprocate?
            ensure!(
                client.server_add_peer(&self.our_features)?,
                "server does not reciprocate"
            );
        }

        Ok(features)
    }

    fn verify_compatibility(&self, features: &ServerFeatures) -> Result<()> {
        ensure!(
            features.genesis_hash == self.our_features.genesis_hash,
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
    fn resolve(host: &str) -> Result<Self> {
        Ok(if host.ends_with(".onion") {
            ServerAddr::Onion(host.into())
        } else if let Ok(ip) = IpAddr::from_str(host) {
            ServerAddr::Clearnet(ip)
        } else {
            let ip = format!("{}:1", host)
                .to_socket_addrs()
                .chain_err(|| "hostname resolution failed")?
                .next()
                .chain_err(|| "hostname resolution failed")?
                .ip();
            ServerAddr::Clearnet(ip)
        })
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
    fn new(
        addr: ServerAddr,
        hostname: Hostname,
        service: Service,
        added_by: Option<IpAddr>,
    ) -> Self {
        HealthCheck {
            addr,
            hostname,
            service,
            is_default: added_by.is_none(),
            added_by,
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

fn is_remote_addr(addr: &ServerAddr) -> bool {
    match addr {
        ServerAddr::Onion(_) => true,
        ServerAddr::Clearnet(ip) => {
            !ip.is_loopback()
                && !ip.is_unspecified()
                && !ip.is_multicast()
                && !match ip {
                    IpAddr::V4(ipv4) => ipv4.is_private(),
                    IpAddr::V6(_) => false,
                }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chain::genesis_hash;
    use crate::chain::Network;
    use std::time;

    const PROTOCOL_VERSION: ProtocolVersion = ProtocolVersion::new(1, 4);

    #[test]
    fn test() -> Result<()> {
        stderrlog::new().verbosity(4).init().unwrap();

        let features = ServerFeatures {
            hosts: serde_json::from_str("{\"test.foobar.example\":{\"tcp_port\":60002}}").unwrap(),
            server_version: format!("electrs-esplora 9"),
            genesis_hash: genesis_hash(Network::Testnet),
            protocol_min: PROTOCOL_VERSION,
            protocol_max: PROTOCOL_VERSION,
            hash_function: "sha256".into(),
            pruning: None,
        };
        let discovery = Arc::new(DiscoveryManager::new(
            Network::Testnet,
            features,
            PROTOCOL_VERSION,
            false,
            None,
        ));
        discovery.add_default_server(
            "electrum.blockstream.info".into(),
            vec![Service::Tcp(60001)],
        ).unwrap();
        discovery.add_default_server("testnet.hsmiths.com".into(), vec![Service::Ssl(53012)]).unwrap();
        discovery.add_default_server(
            "tn.not.fyi".into(),
            vec![Service::Tcp(55001), Service::Ssl(55002)],
        ).unwrap();
        discovery.add_default_server(
            "electrum.blockstream.info".into(),
            vec![Service::Tcp(60001), Service::Ssl(60002)],
        ).unwrap();
        discovery.add_default_server(
            "explorerzydxu5ecjrkwceayqybizmpjjznk5izmitf2modhcusuqlid.onion".into(),
            vec![Service::Tcp(143)],
        ).unwrap();

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
