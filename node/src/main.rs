// Copyright(C) Facebook, Inc. and its affiliates.
use anyhow::{Context, Result};
use clap::{crate_name, crate_version, App, AppSettings, ArgMatches, SubCommand};
use config::BlsKeyPair;
use config::Clan;
use config::Export as _;
use config::Import as _;
use config::{Comm, Committee, KeyPair, Parameters};
use consensus::Consensus;
use crypto::combine_keys;
use env_logger::Env;
use primary::{Certificate, Primary};
use store::Store;
use tokio::sync::mpsc::{channel, Receiver};

/// The default channel capacity.
pub const CHANNEL_CAPACITY: usize = 1_000;

#[tokio::main]
async fn main() -> Result<()> {
    let matches = App::new(crate_name!())
        .version(crate_version!())
        .about("A research implementation of Narwhal and Tusk.")
        .args_from_usage("-v... 'Sets the level of verbosity'")
        .subcommand(
            SubCommand::with_name("generate_keys")
                .about("Print a fresh key pair to file")
                .args_from_usage("--filename=<FILE> 'The file where to print the new key pair'"),
        )
        .subcommand(
            SubCommand::with_name("generate_bls_keys")
                .about("Print a fresh bls key pair to file")
                .arg_from_usage("--nodes=<INT> 'total number of nodes in network'")
                .arg_from_usage("--threshold=<INT>  'threshold number of keys require to verify'")
                .arg_from_usage("--path=<String>  'Path for storing blskeys'")
                .arg_from_usage("--node_id_to_start=<INT> 'gives node id to save files'"),
        )
        .subcommand(
            SubCommand::with_name("run")
                .about("Run a node")
                .args_from_usage("--edkeys=<FILE> 'The file containing the node keys'")
                .args_from_usage("--blskeys=<FILE> 'The file containing the node keys'")
                .args_from_usage("--committee=<FILE> 'The file containing committee information'")
                .args_from_usage("--parameters=[FILE] 'The file containing the node parameters'")
                .args_from_usage("--store=<PATH> 'The path where to create the data store'")
                .subcommand(SubCommand::with_name("primary").about("Run a single primary"))
                .subcommand(
                    SubCommand::with_name("worker")
                        .about("Run a single worker")
                        .args_from_usage("--id=<INT> 'The worker id'"),
                )
                .setting(AppSettings::SubcommandRequiredElseHelp),
        )
        .setting(AppSettings::SubcommandRequiredElseHelp)
        .get_matches();

    let log_level = match matches.occurrences_of("v") {
        0 => "error",
        1 => "warn",
        2 => "info",
        3 => "debug",
        _ => "trace",
    };
    let mut logger = env_logger::Builder::from_env(Env::default().default_filter_or(log_level));
    #[cfg(feature = "benchmark")]
    logger.format_timestamp_millis();
    logger.init();

    match matches.subcommand() {
        ("generate_keys", Some(sub_matches)) => KeyPair::new()
            .export(sub_matches.value_of("filename").unwrap())
            .context("Failed to generate key pair")?,
        ("generate_bls_keys", Some(sub_matches)) => BlsKeyPair::new(
            sub_matches
                .value_of("nodes")
                .unwrap()
                .parse::<usize>()
                .unwrap(),
            sub_matches
                .value_of("threshold")
                .unwrap()
                .parse::<usize>()
                .unwrap(),
            sub_matches
                .value_of("path")
                .unwrap()
                .parse::<String>()
                .unwrap(),
            sub_matches
                .value_of("node_id_to_start")
                .unwrap()
                .parse::<usize>()
                .unwrap(),
        ),
        ("run", Some(sub_matches)) => run(sub_matches).await?,
        _ => unreachable!(),
    }
    Ok(())
}

// Runs either a worker or a primary.
async fn run(matches: &ArgMatches<'_>) -> Result<()> {
    let ed_key_file = matches.value_of("edkeys").unwrap();
    let bls_key_file = matches.value_of("blskeys").unwrap();
    let committee_file = matches.value_of("committee").unwrap();
    let parameters_file = matches.value_of("parameters");
    let store_path = matches.value_of("store").unwrap();

    // Read the committee and node's keypair from file.
    let ed_keypair = KeyPair::import(ed_key_file).context("Failed to load the node's keypair")?;
    let name = ed_keypair.name;
    let bls_keypair =
        BlsKeyPair::import(bls_key_file).context("Failed to load the node's keypair")?;

    //fetching committee
    let comm = Comm::import(committee_file).context("Failed to load the committee information")?;
    let my_clan_id: usize = comm.authorities.get(&name).unwrap().clan_id;

    let committee = Committee::new(comm.authorities, &name, my_clan_id);
    //fetching clanmy_clan_id
    let clan = Clan::create_clan_from_committee(&committee, my_clan_id, &name)
        .context("Failed to load the clan information")?;

    let mut sorted_keys = committee.get_bls_public_keys();
    sorted_keys.sort();

    let combined_pubkey = combine_keys(&sorted_keys);

    // Load default parameters if none are specified.
    let parameters = match parameters_file {
        Some(filename) => {
            Parameters::import(filename).context("Failed to load the node's parameters")?
        }
        None => Parameters::default(),
    };

    // Make the data store.
    let store = Store::new(store_path).context("Failed to create a store")?;

    // Channels the sequence of certificates.
    let (tx_output, rx_output) = channel(CHANNEL_CAPACITY);

    // Check whether to run a primary, a worker, or an entire authority.
    match matches.subcommand() {
        // Spawn the primary and consensus core.
        ("primary", _) => {
            let (tx_new_certificates, rx_new_certificates) = channel(CHANNEL_CAPACITY);
            let (tx_feedback, rx_feedback) = channel(CHANNEL_CAPACITY);
            let (tx_consensus_header_msg, rx_consensus_header_msg) = channel(CHANNEL_CAPACITY);

            Primary::spawn(
                ed_keypair,
                bls_keypair,
                committee.clone(),
                clan.clone(),
                sorted_keys,
                combined_pubkey,
                parameters.clone(),
                store,
                /* tx_consensus */ tx_new_certificates,
                /* rx_consensus */ rx_feedback,
                tx_consensus_header_msg,
                parameters.leaders_per_round,
            );
            Consensus::spawn(
                committee,
                parameters.gc_depth,
                /* rx_primary */ rx_new_certificates,
                rx_consensus_header_msg,
                /* tx_primary */ tx_feedback,
                tx_output,
                parameters.leaders_per_round,
            );
        }

        _ => unreachable!(),
    }

    // Analyze the consensus' output.
    analyze(rx_output).await;

    // If this expression is reached, the program ends and all other tasks terminate.
    unreachable!();
}

/// Receives an ordered list of certificates and apply any application-specific logic.
async fn analyze(mut rx_output: Receiver<Certificate>) {
    while let Some(_certificate) = rx_output.recv().await {
        // NOTE: Here goes the application logic.
    }
}
