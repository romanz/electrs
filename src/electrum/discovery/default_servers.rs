use crate::chain::Network;
use crate::electrum::discovery::{DiscoveryManager, Service};

pub fn add_default_servers(discovery: &DiscoveryManager, network: Network) {
    match network {
        Network::Bitcoin => {
            discovery
                .add_default_server(
                    "3smoooajg7qqac2y.onion".into(),
                    vec![Service::Tcp(50001), Service::Ssl(50002)],
                )
                .ok();
            discovery
                .add_default_server(
                    "81-7-10-251.blue.kundencontroller.de".into(),
                    vec![Service::Ssl(50002)],
                )
                .ok();
            discovery
                .add_default_server(
                    "E-X.not.fyi".into(),
                    vec![Service::Tcp(50001), Service::Ssl(50002)],
                )
                .ok();
            discovery
                .add_default_server(
                    "VPS.hsmiths.com".into(),
                    vec![Service::Tcp(50001), Service::Ssl(50002)],
                )
                .ok();
            discovery
                .add_default_server(
                    "b.ooze.cc".into(),
                    vec![Service::Tcp(50001), Service::Ssl(50002)],
                )
                .ok();
            discovery
                .add_default_server(
                    "bauerjda5hnedjam.onion".into(),
                    vec![Service::Tcp(50001), Service::Ssl(50002)],
                )
                .ok();
            discovery
                .add_default_server(
                    "bauerjhejlv6di7s.onion".into(),
                    vec![Service::Tcp(50001), Service::Ssl(50002)],
                )
                .ok();
            discovery
                .add_default_server(
                    "bitcoin.corgi.party".into(),
                    vec![Service::Tcp(50001), Service::Ssl(50002)],
                )
                .ok();
            discovery
                .add_default_server(
                    "bitcoin3nqy3db7c.onion".into(),
                    vec![Service::Tcp(50001), Service::Ssl(50002)],
                )
                .ok();
            discovery
                .add_default_server(
                    "bitcoins.sk".into(),
                    vec![Service::Tcp(50001), Service::Ssl(50002)],
                )
                .ok();
            discovery
                .add_default_server(
                    "btc.cihar.com".into(),
                    vec![Service::Tcp(50001), Service::Ssl(50002)],
                )
                .ok();
            discovery
                .add_default_server(
                    "btc.xskyx.net".into(),
                    vec![Service::Tcp(50001), Service::Ssl(50002)],
                )
                .ok();
            discovery
                .add_default_server(
                    "currentlane.lovebitco.in".into(),
                    vec![Service::Tcp(50001), Service::Ssl(50002)],
                )
                .ok();
            discovery
                .add_default_server(
                    "daedalus.bauerj.eu".into(),
                    vec![Service::Tcp(50001), Service::Ssl(50002)],
                )
                .ok();
            discovery
                .add_default_server(
                    "electrum.jochen-hoenicke.de".into(),
                    vec![Service::Tcp(50003), Service::Ssl(50005)],
                )
                .ok();
            discovery
                .add_default_server(
                    "dragon085.startdedicated.de".into(),
                    vec![Service::Ssl(50002)],
                )
                .ok();
            discovery
                .add_default_server(
                    "e-1.claudioboxx.com".into(),
                    vec![Service::Tcp(50001), Service::Ssl(50002)],
                )
                .ok();
            discovery
                .add_default_server(
                    "e.keff.org".into(),
                    vec![Service::Tcp(50001), Service::Ssl(50002)],
                )
                .ok();
            discovery
                .add_default_server(
                    "electrum-server.ninja".into(),
                    vec![Service::Tcp(50001), Service::Ssl(50002)],
                )
                .ok();
            discovery
                .add_default_server(
                    "electrum-unlimited.criptolayer.net".into(),
                    vec![Service::Ssl(50002)],
                )
                .ok();
            discovery
                .add_default_server(
                    "electrum.eff.ro".into(),
                    vec![Service::Tcp(50001), Service::Ssl(50002)],
                )
                .ok();
            discovery
                .add_default_server(
                    "electrum.festivaldelhumor.org".into(),
                    vec![Service::Tcp(50001), Service::Ssl(50002)],
                )
                .ok();
            discovery
                .add_default_server(
                    "electrum.hsmiths.com".into(),
                    vec![Service::Tcp(50001), Service::Ssl(50002)],
                )
                .ok();
            discovery
                .add_default_server(
                    "electrum.leblancnet.us".into(),
                    vec![Service::Tcp(50001), Service::Ssl(50002)],
                )
                .ok();
            discovery
                .add_default_server("electrum.mindspot.org".into(), vec![Service::Ssl(50002)])
                .ok();
            discovery
                .add_default_server(
                    "electrum.qtornado.com".into(),
                    vec![Service::Tcp(50001), Service::Ssl(50002)],
                )
                .ok();
            discovery
                .add_default_server("electrum.taborsky.cz".into(), vec![Service::Ssl(50002)])
                .ok();
            discovery
                .add_default_server(
                    "electrum.villocq.com".into(),
                    vec![Service::Tcp(50001), Service::Ssl(50002)],
                )
                .ok();
            discovery
                .add_default_server(
                    "electrum2.eff.ro".into(),
                    vec![Service::Tcp(50001), Service::Ssl(50002)],
                )
                .ok();
            discovery
                .add_default_server(
                    "electrum2.villocq.com".into(),
                    vec![Service::Tcp(50001), Service::Ssl(50002)],
                )
                .ok();
            discovery
                .add_default_server(
                    "electrumx.bot.nu".into(),
                    vec![Service::Tcp(50001), Service::Ssl(50002)],
                )
                .ok();
            discovery
                .add_default_server(
                    "electrumx.ddns.net".into(),
                    vec![Service::Tcp(50001), Service::Ssl(50002)],
                )
                .ok();
            discovery
                .add_default_server("electrumx.ftp.sh".into(), vec![Service::Ssl(50002)])
                .ok();
            discovery
                .add_default_server(
                    "electrumx.ml".into(),
                    vec![Service::Tcp(50001), Service::Ssl(50002)],
                )
                .ok();
            discovery
                .add_default_server(
                    "electrumx.soon.it".into(),
                    vec![Service::Tcp(50001), Service::Ssl(50002)],
                )
                .ok();
            discovery
                .add_default_server("electrumxhqdsmlu.onion".into(), vec![Service::Tcp(50001)])
                .ok();
            discovery
                .add_default_server(
                    "elx01.knas.systems".into(),
                    vec![Service::Tcp(50001), Service::Ssl(50002)],
                )
                .ok();
            discovery
                .add_default_server(
                    "enode.duckdns.org".into(),
                    vec![Service::Tcp(50001), Service::Ssl(50002)],
                )
                .ok();
            discovery
                .add_default_server(
                    "fedaykin.goip.de".into(),
                    vec![Service::Tcp(50001), Service::Ssl(50002)],
                )
                .ok();
            discovery
                .add_default_server(
                    "fn.48.org".into(),
                    vec![Service::Tcp(50003), Service::Ssl(50002)],
                )
                .ok();
            discovery
                .add_default_server(
                    "helicarrier.bauerj.eu".into(),
                    vec![Service::Tcp(50001), Service::Ssl(50002)],
                )
                .ok();
            discovery
                .add_default_server(
                    "hsmiths4fyqlw5xw.onion".into(),
                    vec![Service::Tcp(50001), Service::Ssl(50002)],
                )
                .ok();
            discovery
                .add_default_server(
                    "hsmiths5mjk6uijs.onion".into(),
                    vec![Service::Tcp(50001), Service::Ssl(50002)],
                )
                .ok();
            discovery
                .add_default_server(
                    "icarus.tetradrachm.net".into(),
                    vec![Service::Tcp(50001), Service::Ssl(50002)],
                )
                .ok();
            discovery
                .add_default_server(
                    "electrum.emzy.de".into(),
                    vec![Service::Tcp(50001), Service::Ssl(50002)],
                )
                .ok();
            discovery
                .add_default_server(
                    "ndnd.selfhost.eu".into(),
                    vec![Service::Tcp(50001), Service::Ssl(50002)],
                )
                .ok();
            discovery
                .add_default_server("ndndword5lpb7eex.onion".into(), vec![Service::Tcp(50001)])
                .ok();
            discovery
                .add_default_server(
                    "orannis.com".into(),
                    vec![Service::Tcp(50001), Service::Ssl(50002)],
                )
                .ok();
            discovery
                .add_default_server(
                    "ozahtqwp25chjdjd.onion".into(),
                    vec![Service::Tcp(50001), Service::Ssl(50002)],
                )
                .ok();
            discovery
                .add_default_server(
                    "qtornadoklbgdyww.onion".into(),
                    vec![Service::Tcp(50001), Service::Ssl(50002)],
                )
                .ok();
            discovery
                .add_default_server("rbx.curalle.ovh".into(), vec![Service::Ssl(50002)])
                .ok();
            discovery
                .add_default_server("s7clinmo4cazmhul.onion".into(), vec![Service::Tcp(50001)])
                .ok();
            discovery
                .add_default_server(
                    "tardis.bauerj.eu".into(),
                    vec![Service::Tcp(50001), Service::Ssl(50002)],
                )
                .ok();
            discovery
                .add_default_server("technetium.network".into(), vec![Service::Ssl(50002)])
                .ok();
            discovery
                .add_default_server(
                    "tomscryptos.com".into(),
                    vec![Service::Tcp(50001), Service::Ssl(50002)],
                )
                .ok();
            discovery
                .add_default_server(
                    "ulrichard.ch".into(),
                    vec![Service::Tcp(50001), Service::Ssl(50002)],
                )
                .ok();
            discovery
                .add_default_server(
                    "vmd27610.contaboserver.net".into(),
                    vec![Service::Tcp(50001), Service::Ssl(50002)],
                )
                .ok();
            discovery
                .add_default_server(
                    "vmd30612.contaboserver.net".into(),
                    vec![Service::Tcp(50001), Service::Ssl(50002)],
                )
                .ok();
            discovery
                .add_default_server(
                    "wsw6tua3xl24gsmi264zaep6seppjyrkyucpsmuxnjzyt3f3j6swshad.onion".into(),
                    vec![Service::Tcp(50001), Service::Ssl(50002)],
                )
                .ok();
            discovery
                .add_default_server(
                    "xray587.startdedicated.de".into(),
                    vec![Service::Ssl(50002)],
                )
                .ok();
            discovery
                .add_default_server(
                    "yuio.top".into(),
                    vec![Service::Tcp(50001), Service::Ssl(50002)],
                )
                .ok();
            discovery
                .add_default_server(
                    "bitcoin.dragon.zone".into(),
                    vec![Service::Tcp(50003), Service::Ssl(50004)],
                )
                .ok();
            discovery
                .add_default_server(
                    "ecdsa.net".into(),
                    vec![Service::Tcp(50001), Service::Ssl(110)],
                )
                .ok();
            discovery
                .add_default_server("btc.usebsv.com".into(), vec![Service::Ssl(50006)])
                .ok();
            discovery
                .add_default_server(
                    "e2.keff.org".into(),
                    vec![Service::Tcp(50001), Service::Ssl(50002)],
                )
                .ok();
            discovery
                .add_default_server("electrum.hodlister.co".into(), vec![Service::Ssl(50002)])
                .ok();
            discovery
                .add_default_server("electrum3.hodlister.co".into(), vec![Service::Ssl(50002)])
                .ok();
            discovery
                .add_default_server("electrum5.hodlister.co".into(), vec![Service::Ssl(50002)])
                .ok();
            discovery
                .add_default_server(
                    "electrumx.electricnewyear.net".into(),
                    vec![Service::Ssl(50002)],
                )
                .ok();
            discovery
                .add_default_server(
                    "fortress.qtornado.com".into(),
                    vec![Service::Tcp(50001), Service::Ssl(443)],
                )
                .ok();
            discovery
                .add_default_server(
                    "green-gold.westeurope.cloudapp.azure.com".into(),
                    vec![Service::Tcp(56001), Service::Ssl(56002)],
                )
                .ok();
            discovery
                .add_default_server(
                    "electrumx.erbium.eu".into(),
                    vec![Service::Tcp(50001), Service::Ssl(50002)],
                )
                .ok();
        }
        Network::Testnet => {
            discovery
                .add_default_server(
                    "hsmithsxurybd7uh.onion".into(),
                    vec![Service::Tcp(53011), Service::Ssl(53012)],
                )
                .ok();
            discovery
                .add_default_server(
                    "testnet.hsmiths.com".into(),
                    vec![Service::Tcp(53011), Service::Ssl(53012)],
                )
                .ok();
            discovery
                .add_default_server(
                    "testnet.qtornado.com".into(),
                    vec![Service::Tcp(51001), Service::Ssl(51002)],
                )
                .ok();
            discovery
                .add_default_server(
                    "testnet1.bauerj.eu".into(),
                    vec![Service::Tcp(50001), Service::Ssl(50002)],
                )
                .ok();
            discovery
                .add_default_server(
                    "tn.not.fyi".into(),
                    vec![Service::Tcp(55001), Service::Ssl(55002)],
                )
                .ok();
            discovery
                .add_default_server(
                    "bitcoin.cluelessperson.com".into(),
                    vec![Service::Tcp(51001), Service::Ssl(51002)],
                )
                .ok();
        }

        _ => (),
    }
}
