use super::{WalletTrait, WalletType};
use clap::{ArgAction, Parser};
use ethers::{
    middleware::SignerMiddleware,
    prelude::{Middleware, Signer},
    signers::{HDPath as LedgerHDPath, Ledger, LocalWallet, Trezor, TrezorHDPath},
    types::Address,
};
use eyre::{Context, Result};
use foundry_common::RetryProvider;
use foundry_config::Config;
use itertools::izip;
use serde::Serialize;
use std::{
    collections::{HashMap, HashSet},
    iter::repeat,
    sync::Arc,
};

macro_rules! get_wallets {
    ($id:ident, [ $($wallets:expr),+ ], $call:expr) => {
        $(
            if let Some($id) = $wallets {
                $call;
            }
        )+
    };
}

macro_rules! collect_addresses {
    ($local:expr, $unused:expr, $addresses:expr, $addr:expr, $wallet:expr) => {
        if $addresses.contains(&$addr) {
            $addresses.remove(&$addr);

            $local.insert($addr, $wallet);

            if $addresses.is_empty() {
                return Ok($local)
            }
        } else {
            // Just to show on error.
            $unused.push($addr);
        }
    };
}

/// A macro that initializes multiple wallets
///
/// Should be used with a [`MultiWallet`] instance
macro_rules! create_hw_wallets {
    ($self:ident, $chain_id:ident ,$get_wallet:ident, $wallets:ident) => {
        let mut $wallets = vec![];

        if let Some(hd_paths) = &$self.hd_paths {
            for path in hd_paths {
                if let Some(hw) = $self.$get_wallet($chain_id, Some(path), None).await? {
                    $wallets.push(hw);
                }
            }
        }

        if let Some(mnemonic_indexes) = &$self.mnemonic_indexes {
            for index in mnemonic_indexes {
                if let Some(hw) = $self.$get_wallet($chain_id, None, Some(*index as usize)).await? {
                    $wallets.push(hw);
                }
            }
        }

        if $wallets.is_empty() {
            if let Some(hw) = $self.$get_wallet($chain_id, None, Some(0)).await? {
                $wallets.push(hw);
            }
        }
    };
}

#[derive(Parser, Debug, Clone, Serialize, Default)]
#[cfg_attr(not(doc), allow(missing_docs))]
#[cfg_attr(
    doc,
    doc = r#"
The wallet options can either be:
1. Ledger
2. Trezor
3. Mnemonics (via file path)
4. Keystores (via file path)
5. Private Keys (cleartext in CLI)
6. Private Keys (interactively via secure prompt)
"#
)]
pub struct MultiWallet {
    #[clap(
        long,
        short,
        help_heading = "WALLET OPTIONS - RAW",
        help = "Open an interactive prompt to enter your private key. Takes a value for the number of keys to enter",
        default_value = "0",
        value_name = "NUM"
    )]
    pub interactives: u32,

    #[clap(
        long = "private-keys",
        help_heading = "WALLET OPTIONS - RAW",
        help = "Use the provided private keys.",
        value_name = "RAW_PRIVATE_KEYS",
        value_parser = foundry_common::clap_helpers::strip_0x_prefix,
        action = ArgAction::Append,
    )]
    pub private_keys: Option<Vec<String>>,

    #[clap(
        long = "private-key",
        help_heading = "WALLET OPTIONS - RAW",
        help = "Use the provided private key.",
        conflicts_with = "private_keys",
        value_name = "RAW_PRIVATE_KEY",
        value_parser = foundry_common::clap_helpers::strip_0x_prefix,
    )]
    pub private_key: Option<String>,

    #[clap(
        long = "mnemonics",
        alias = "mnemonic-paths",
        help_heading = "WALLET OPTIONS - RAW",
        help = "Use the mnemonic phrases or mnemonic files at the specified paths.",
        value_name = "PATHS",
        action = ArgAction::Append,
    )]
    pub mnemonics: Option<Vec<String>>,

    #[clap(
        long = "mnemonic-passphrases",
        help_heading = "WALLET OPTIONS - RAW",
        help = "Use a BIP39 passphrases for the mnemonic.",
        value_name = "PASSPHRASE",
        action = ArgAction::Append,
    )]
    pub mnemonic_passphrases: Option<Vec<String>>,

    #[clap(
        long = "mnemonic-derivation-paths",
        alias = "hd-paths",
        help_heading = "WALLET OPTIONS - RAW",
        help = "The wallet derivation path. Works with both --mnemonic-path and hardware wallets.",
        value_name = "PATHS",
        action = ArgAction::Append,
    )]
    pub hd_paths: Option<Vec<String>>,

    #[clap(
        long = "mnemonic-indexes",
        conflicts_with = "hd_paths",
        help_heading = "WALLET OPTIONS - RAW",
        help = "Use the private key from the given mnemonic index. Used with --mnemonic-paths.",
        default_value = "0",
        value_name = "INDEXES",
        action = ArgAction::Append,
    )]
    pub mnemonic_indexes: Option<Vec<u32>>,

    #[clap(
        env = "ETH_KEYSTORE",
        long = "keystore",
        visible_alias = "keystores",
        help_heading = "WALLET OPTIONS - KEYSTORE",
        help = "Use the keystore in the given folder or file.",
        action = ArgAction::Append,
        value_name = "PATHS",
    )]
    pub keystore_paths: Option<Vec<String>>,

    #[clap(
        long = "password",
        help_heading = "WALLET OPTIONS - KEYSTORE",
        help = "The keystore password. Used with --keystore.",
        requires = "keystore_paths",
        value_name = "PASSWORDS",
        action = ArgAction::Append,
    )]
    pub keystore_passwords: Option<Vec<String>>,

    #[clap(
        short,
        long = "ledger",
        help_heading = "WALLET OPTIONS - HARDWARE WALLET",
        help = "Use a Ledger hardware wallet."
    )]
    pub ledger: bool,

    #[clap(
        short,
        long = "trezor",
        help_heading = "WALLET OPTIONS - HARDWARE WALLET",
        help = "Use a Trezor hardware wallet."
    )]
    pub trezor: bool,

    #[clap(
        env = "ETH_FROM",
        short = 'a',
        long = "froms",
        help_heading = "WALLET OPTIONS - REMOTE",
        help = "The sender account.",
        value_name = "ADDRESSES",
        action = ArgAction::Append,
    )]
    pub froms: Option<Vec<Address>>,
}

impl WalletTrait for MultiWallet {
    fn sender(&self) -> Option<Address> {
        self.froms.as_ref()?.first().copied()
    }
}

impl MultiWallet {
    /// Given a list of addresses, it finds all the associated wallets if they exist. Throws an
    /// error, if it can't find all.
    pub async fn find_all(
        &self,
        provider: Arc<RetryProvider>,
        mut addresses: HashSet<Address>,
        script_wallets: Vec<LocalWallet>,
    ) -> Result<HashMap<Address, WalletType>> {
        println!("\n###\nFinding wallets for all the necessary addresses...");
        let chain = provider.get_chainid().await?.as_u64();

        let mut local_wallets = HashMap::new();
        let mut unused_wallets = vec![];

        let script_wallets_fn = || -> Result<Option<Vec<LocalWallet>>> {
            if !script_wallets.is_empty() {
                return Ok(Some(script_wallets))
            }
            Ok(None)
        };

        get_wallets!(
            wallets,
            [
                self.trezors(chain).await?,
                self.ledgers(chain).await?,
                self.private_keys()?,
                self.interactives()?,
                self.mnemonics()?,
                self.keystores()?,
                script_wallets_fn()?
            ],
            for wallet in wallets.into_iter() {
                let address = wallet.address();
                let wallet = wallet.with_chain_id(chain);
                let wallet: WalletType = SignerMiddleware::new(provider.clone(), wallet).into();

                collect_addresses!(local_wallets, unused_wallets, addresses, address, wallet);
            }
        );

        let mut error_msg = String::new();

        // This is an actual used address
        if addresses.contains(&Config::DEFAULT_SENDER) {
            error_msg += "\nYou seem to be using Foundry's default sender. Be sure to set your own --sender.\n";
        }

        unused_wallets.extend(local_wallets.into_keys());
        eyre::bail!(
            "{}No associated wallet for addresses: {:?}. Unlocked wallets: {:?}",
            error_msg,
            addresses,
            unused_wallets
        )
    }

    pub fn interactives(&self) -> Result<Option<Vec<LocalWallet>>> {
        if self.interactives != 0 {
            let mut wallets = vec![];
            for _ in 0..self.interactives {
                wallets.push(self.get_from_interactive()?);
            }
            return Ok(Some(wallets))
        }
        Ok(None)
    }

    pub fn private_keys(&self) -> Result<Option<Vec<LocalWallet>>> {
        if let Some(private_keys) = &self.private_keys {
            let mut wallets = vec![];
            for private_key in private_keys.iter() {
                wallets.push(self.get_from_private_key(private_key.trim())?);
            }
            return Ok(Some(wallets))
        }
        Ok(None)
    }

    /// Returns all wallets read from the provided keystores arguments
    ///
    /// Returns `Ok(None)` if no keystore provided.
    pub fn keystores(&self) -> Result<Option<Vec<LocalWallet>>> {
        if let Some(keystore_paths) = &self.keystore_paths {
            let mut wallets = vec![];

            let mut passwords: Vec<Option<String>> = self
                .keystore_passwords
                .clone()
                .unwrap_or_default()
                .iter()
                .map(|pw| Some(pw.clone()))
                .collect();

            if passwords.is_empty() {
                passwords = vec![None; keystore_paths.len()]
            } else if passwords.len() != keystore_paths.len() {
                eyre::bail!("Keystore passwords don't have the same length as keystore paths.");
            }

            for (path, password) in keystore_paths.iter().zip(passwords) {
                wallets.push(self.get_from_keystore(Some(path), password.as_ref())?.unwrap());
            }
            return Ok(Some(wallets))
        }
        Ok(None)
    }

    pub fn mnemonics(&self) -> Result<Option<Vec<LocalWallet>>> {
        if let Some(ref mnemonics) = self.mnemonics {
            let mut wallets = vec![];
            let hd_paths: Vec<_> = if let Some(ref hd_paths) = self.hd_paths {
                hd_paths.iter().map(Some).collect()
            } else {
                repeat(None).take(mnemonics.len()).collect()
            };
            let mnemonic_passphrases: Vec<_> =
                if let Some(ref mnemonic_passphrases) = self.mnemonic_passphrases {
                    mnemonic_passphrases.iter().map(Some).collect()
                } else {
                    repeat(None).take(mnemonics.len()).collect()
                };
            let mnemonic_indexes: Vec<_> = if let Some(ref mnemonic_indexes) = self.mnemonic_indexes
            {
                mnemonic_indexes.to_vec()
            } else {
                repeat(0).take(mnemonics.len()).collect()
            };
            for (mnemonic, mnemonic_passphrase, hd_path, mnemonic_index) in
                izip!(mnemonics, mnemonic_passphrases, hd_paths, mnemonic_indexes)
            {
                wallets.push(self.get_from_mnemonic(
                    mnemonic,
                    mnemonic_passphrase,
                    hd_path,
                    mnemonic_index,
                )?)
            }
            return Ok(Some(wallets))
        }
        Ok(None)
    }

    pub async fn ledgers(&self, chain_id: u64) -> Result<Option<Vec<Ledger>>> {
        if self.ledger {
            let mut args = self.clone();

            if let Some(paths) = &args.hd_paths {
                if paths.len() > 1 {
                    eyre::bail!("Ledger only supports one signer.");
                }
                args.mnemonic_indexes = None;
            }

            create_hw_wallets!(args, chain_id, get_from_ledger, wallets);
            return Ok(Some(wallets))
        }
        Ok(None)
    }

    pub async fn trezors(&self, chain_id: u64) -> Result<Option<Vec<Trezor>>> {
        if self.trezor {
            create_hw_wallets!(self, chain_id, get_from_trezor, wallets);
            return Ok(Some(wallets))
        }
        Ok(None)
    }

    async fn get_from_trezor(
        &self,
        chain_id: u64,
        hd_path: Option<&str>,
        mnemonic_index: Option<usize>,
    ) -> Result<Option<Trezor>> {
        let derivation = match &hd_path {
            Some(hd_path) => TrezorHDPath::Other(hd_path.to_string()),
            None => TrezorHDPath::TrezorLive(mnemonic_index.unwrap_or(0)),
        };

        Ok(Some(Trezor::new(derivation, chain_id, None).await?))
    }

    async fn get_from_ledger(
        &self,
        chain_id: u64,
        hd_path: Option<&str>,
        mnemonic_index: Option<usize>,
    ) -> Result<Option<Ledger>> {
        let derivation = match hd_path {
            Some(hd_path) => LedgerHDPath::Other(hd_path.to_string()),
            None => LedgerHDPath::LedgerLive(mnemonic_index.unwrap_or(0)),
        };

        Ok(Some(Ledger::new(derivation, chain_id).await.wrap_err("Ledger device not available.")?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_keystore_args() {
        let args: MultiWallet =
            MultiWallet::parse_from(["foundry-cli", "--keystores", "my/keystore/path"]);
        assert_eq!(args.keystore_paths, Some(vec!["my/keystore/path".to_string()]));

        std::env::set_var("ETH_KEYSTORE", "MY_KEYSTORE");
        let args: MultiWallet = MultiWallet::parse_from(["foundry-cli"]);
        assert_eq!(args.keystore_paths, Some(vec!["MY_KEYSTORE".to_string()]));

        std::env::remove_var("ETH_KEYSTORE");
    }
}
