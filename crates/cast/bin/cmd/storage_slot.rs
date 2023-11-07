use cast::SimpleCast;
use clap::Parser;
use eyre::Result;
use foundry_cli::opts::{CompilerArgs,CoreBuildArgs, EtherscanOpts};
use foundry_common::{
    compile,
    fs,
};
use foundry_config::Config;
use foundry_compilers::{
    artifacts::output_selection::ContractOutputSelection, info::ContractInfo, utils::canonicalize,
};
use std::path::PathBuf;


/// CLI arguments for `cast storage`.
#[derive(Debug, Clone, Parser)]
pub struct StorageSlotArgs {
    /// The contract's address.
    address: String,

    #[clap(flatten)]
    etherscan: EtherscanOpts,
}


impl StorageSlotArgs {
    pub async fn run(self) -> Result<()> {
            let config = Config::from(&self.etherscan);
            let chain = config.chain_id.unwrap_or_default();
            let api_key = config.get_etherscan_api_key(Some(chain)).unwrap_or_default();
            let chain = chain.named()?;
            let cache_dir = PathBuf::from("./storage_slot_cache");
            if !cache_dir.exists() {
                fs::create_dir_all(&cache_dir)?;
            }
            let meta = SimpleCast::expand_etherscan_source_to_directory_and_return_metadata(
                chain,
                self.address,
                api_key,
                cache_dir.clone(),
            )
            .await?;
            let contract_name = &meta.items[0].contract_name;
            let mut path = cache_dir;
            path.push(contract_name);
            path.push("Contract.sol");
            let build_args = CoreBuildArgs {
                compiler: CompilerArgs {
                    extra_output: vec![ContractOutputSelection::StorageLayout],
                    ..CompilerArgs::default()
                },
                ..CoreBuildArgs::default()
            };
            println!("build_args: {:#?}", build_args);

            let project = build_args.project()?;
            let contract_path = path.to_str().expect("path to be valid utf8").to_string();
            let mut contract = ContractInfo {
                name: contract_name.to_string(),
                path: Some(contract_path),
            };

            let outcome = if let Some(ref mut contract_path) = contract.path {
                let target_path = canonicalize(&*contract_path)?;
                *contract_path = target_path.to_string_lossy().to_string();
                compile::compile_files(&project, vec![target_path], true)
            } else {
                compile::suppress_compile(&project)
            }?;

            // Find the artifact
            let found_artifact = outcome.find_contract(&contract);

            // Unwrap the inner artifact
            let artifact = found_artifact.ok_or_else(|| {
                eyre::eyre!("Could not find artifact `{contract}` in the compiled artifacts")
            })?;
            println!("storage_layout: {:#?}", artifact.storage_layout);
            Ok(())
    }
}
