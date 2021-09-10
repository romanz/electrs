const fs = require('fs')
const toml = require("toml")
const tomlify = require("tomlify-j0.4")

const bcoinUrl = process.env.BITCOIN_URL
const jsonRpcImport = process.env.JSON_RPC_IMPORT
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

    // Bitcoin url has the following format: `http(s)://username:password@address`
    // Find position of `@` to extract `auth`(`username:password`) and `address`
    const doubleSlash = "//"
    const passwordStartIdx = bcoinUrl.indexOf(doubleSlash) + doubleSlash.length
    const atIdx = bcoinUrl.indexOf("@", passwordStartIdx)

    configFile.jsonrpc_import = (jsonRpcImport.toLowerCase() === 'true')
    configFile.auth = bcoinUrl.substring(passwordStartIdx, atIdx)
    configFile.daemon_rpc_addr = bcoinUrl.substring(atIdx+1)
    configFile.db_dir = dbDir
    configFile.network = network
    configFile.electrum_rpc_addr = electrumRpcAddress
    configFile.monitoring_addr = monitoringAddress
    configFile.verbose = parseInt(verbose)

    // tomlify.toToml() writes integer values as a float. Here we format the
    // default rendering to write the config file with integer values as needed.
    const formattedConfigFile = tomlify.toToml(configFile, {
        space: 2,
        replace: (key, value) => {
            // Find keys that match exactly `verbose`.
            if (key.match(/(^verbose)$/)) {
                return value.toFixed(0)
            }

            return false
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
