const fs = require('fs')
const toml = require("toml")
const tomlify = require("tomlify-j0.4")

const jsonRpcImport = process.env.JSON_RPC_IMPORT
const auth = process.env.AUTH
const daemonRpcAddress = process.env.DAEMON_RPC_ADDRESS
const dbDir = process.env.DB_DIR
const network = process.env.NETWORK
const electrumRpcAddress = process.env.ELECTRUM_RPC_ADDRESS
const monitoringAddress = process.env.MONITORING_ADDRESS
const verbose = process.env.VERBOSE

async function provisionElectrs() {
    console.log('###########  Provisioning electrs! ###########')

    console.log('\n<<<<<<<<<<<< Creating electrs config file >>>>>>>>>>>>')
    await createElectrsConfig()

    console.log("\n########### Electrs provisioning complete! ###########")
}

async function createElectrsConfig() {
    const configFile = toml.parse(
        fs.readFileSync("/tmp/electrs-config.toml", "utf8")
    )

    configFile.jsonrpc_import = jsonRpcImport
    configFile.auth = auth
    configFile.daemon_rpc_addr = daemonRpcAddress
    configFile.db_dir = dbDir
    configFile.network = network
    configFile.electrum_rpc_addr = electrumRpcAddress
    configFile.monitoring_addr = monitoringAddress
    configFile.verbose = verbose

    // tomlify.toToml() writes integer values as a float. Here we format the
    // default rendering to write the config file with integer values as needed.
    const formattedConfigFile = tomlify.toToml(configFile, {
        space: 2,
        replace: (key, value) => {
            // Find keys that match exactly `verbose`.
            const matcher = /(^verbose)$/

            return typeof key === "string" && key.match(matcher) ?
                value.toFixed(0) :
                false
        },
    })

    const configWritePath = "/mnt/electrs/config/electrs-config.toml"
    fs.writeFileSync(configWritePath, formattedConfigFile)
    console.log(`electrs config written to ${configWritePath}`)
}

provisionElectrs()
    .catch(error => {
        console.error(error)
        process.exit(1)
    })
    .then(()=> {
        process.exit(0)
    })
