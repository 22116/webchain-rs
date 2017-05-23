//! # JSON RPC module

mod http;
mod serialize;
mod error;

pub use self::error::Error;
use super::contract::Contracts;
use super::core::{self, Address, Transaction};
use super::keystore::{KeyFile, SecurityLevel};
use super::storage::{ChainStorage, Storages, default_path};
use super::util::{ToHex, align_bytes, to_arr, to_u64, trim_hex};
use futures;
use jsonrpc_core::{Error as JsonRpcError, ErrorCode, IoHandler, Params};
use jsonrpc_core::futures::Future;
use jsonrpc_minihttp_server::{DomainsValidation, ServerBuilder, cors};
use log::LogLevel;
use rustc_serialize::json;
use serde_json::Value;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

/// RPC methods
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
pub enum ClientMethod {
    /// [web3_clientVersion](https://github.com/ethereum/wiki/wiki/JSON-RPC#web3_clientversion)
    Version,

    /// [eth_syncing](https://github.com/ethereum/wiki/wiki/JSON-RPC#eth_syncing)
    EthSyncing,

    /// [eth_blockNumber](https://github.com/ethereum/wiki/wiki/JSON-RPC#eth_blocknumber)
    EthBlockNumber,

    /// [eth_accounts](https://github.com/ethereum/wiki/wiki/JSON-RPC#eth_accounts)
    EthAccounts,

    /// [eth_getBalance](https://github.com/ethereum/wiki/wiki/JSON-RPC#eth_getbalance)
    EthGetBalance,

    /// [eth_getTransactionCount](
    /// https://github.com/ethereum/wiki/wiki/JSON-RPC#eth_gettransactioncount)
    EthGetTxCount,

    /// [eth_getTransactionByHash](
    /// https://github.com/ethereumproject/wiki/wiki/JSON-RPC#eth_gettransactionbyhash)
    EthGetTxByHash,

    /// [eth_sendRawTransaction](
    /// https://github.com/paritytech/parity/wiki/JSONRPC-eth-module#eth_sendrawtransaction)
    EthSendRawTransaction,

    /// [eth_call](https://github.com/ethereum/wiki/wiki/JSON-RPC#eth_call)
    EthCall,

    /// [trace_call](https://github.com/ethereumproject/emerald-rs/issues/30#issuecomment-291987132)
    EthTraceCall,
}

/// PRC method's parameters
#[derive(Clone, Debug, PartialEq)]
pub struct MethodParams<'a>(pub ClientMethod, pub &'a Params);

/// Start an HTTP RPC endpoint
pub fn start(addr: &SocketAddr,
             client_addr: &SocketAddr,
             base_path: Option<PathBuf>,
             sec_level: SecurityLevel)
{
    let mut io = IoHandler::default();
    let url = Arc::new(http::AsyncWrapper::new(&format!("http://{}", client_addr)));

    {
        let url = url.clone();

        io.add_async_method("web3_clientVersion",
                            move |p| url.request(&MethodParams(ClientMethod::Version, &p)));
    }

    {
        let url = url.clone();

        io.add_async_method("eth_syncing",
                            move |p| url.request(&MethodParams(ClientMethod::EthSyncing, &p)));
    }

    {
        let url = url.clone();

        io.add_async_method("eth_blockNumber",
                            move |p| url.request(&MethodParams(ClientMethod::EthBlockNumber, &p)));
    }

    {
        let url = url.clone();

        io.add_async_method("eth_accounts",
                            move |p| url.request(&MethodParams(ClientMethod::EthAccounts, &p)));
    }

    {
        let url = url.clone();

        io.add_async_method("eth_getBalance",
                            move |p| url.request(&MethodParams(ClientMethod::EthGetBalance, &p)));
    }

    {
        let url = url.clone();

        io.add_async_method("eth_getTransactionCount",
                            move |p| url.request(&MethodParams(ClientMethod::EthGetTxCount, &p)));
    }

    {
        let url = url.clone();

        io.add_async_method("eth_getTransactionByHash",
                            move |p| url.request(&MethodParams(ClientMethod::EthGetTxByHash, &p)));
    }

    {
        let url = url.clone();

        let callback = move |p| {
            let pk = KeyFile::default().decrypt_key("");
            match Transaction::try_from(&p) {
                Ok(tr) => {
                    url.request(&MethodParams(ClientMethod::EthSendRawTransaction,
                                              &tr.to_raw_params(pk.unwrap())))
                }
                Err(err) => {
                    futures::done(Err(JsonRpcError::invalid_params(err.to_string()))).boxed()
                }
            }
        };

        io.add_async_method("eth_sendTransaction", callback);
    }

    {
        let url = url.clone();

        io.add_async_method("eth_sendRawTransaction", move |p| {
            url.request(&MethodParams(ClientMethod::EthSendRawTransaction, &p))
        });
    }

    {
        let url = url.clone();

        io.add_async_method("eth_call",
                            move |p| url.request(&MethodParams(ClientMethod::EthCall, &p)));
    }

    {
        let url = url.clone();

        io.add_async_method("eth_traceCall",
                            move |p| url.request(&MethodParams(ClientMethod::EthTraceCall, &p)));
    }

    {
        let import_callback = move |p| match Params::parse::<Value>(p) {
            Ok(ref v) => {
                let data = v.as_object().unwrap();
                let kf = data.get("account").unwrap().to_string();

                let name = match data.get("name") {
                    Some(n) => Some(n.to_string()),
                    None => None,
                };

                let descr = match data.get("description") {
                    Some(d) => Some(d.to_string()),
                    None => None,
                };

                match json::decode::<KeyFile>(&kf) {
                    Ok(kf) => {
                        let addr = Address::default().to_string();
                        match kf.flush(&default_path(), None, name, descr) {
                            Ok(_) => futures::done(Ok(Value::String(addr))).boxed(),
                            Err(_) => futures::done(Err(JsonRpcError::internal_error())).boxed(),
                        }
                    }
                    Err(_) => {
                        futures::done(Err(JsonRpcError::invalid_params("Invalid Keyfile data \
                                                                    format")))
                                .boxed()
                    }
                }
            }
            Err(_) => futures::failed(JsonRpcError::invalid_params("Invalid JSON object")).boxed(),
        };

        io.add_async_method("backend_importWallet", import_callback);
    }

    {
        let sec = sec_level.clone();
        let create_callback = move |p| match Params::parse::<Value>(p) {
            Ok(ref v) if v.as_array().is_some() => {
                let passwd = v.as_array().and_then(|arr| arr[0].as_str()).unwrap();

                match KeyFile::new(passwd, &sec) {
                    Ok(kf) => {
                        let addr_res = kf.decrypt_address(passwd);
                        if addr_res.is_err() {
                            return futures::done(Err(JsonRpcError::internal_error())).boxed();
                        }
                        let addr = addr_res.unwrap();

                        match kf.flush(&default_path(), Some(addr), None, None) {
                            Ok(_) => futures::done(Ok(Value::String(addr.to_string()))).boxed(),
                            Err(_) => futures::done(Err(JsonRpcError::internal_error())).boxed(),
                        }
                    }
                    Err(_) => {
                        futures::done(Err(JsonRpcError::invalid_params("Invalid Keyfile data \
                                                                        format")))
                                .boxed()
                    }
                }
            }
            Ok(_) => {
                futures::done(Err(JsonRpcError::invalid_params("Invalid JSON object"))).boxed()
            }
            Err(_) => futures::failed(JsonRpcError::invalid_params("Invalid JSON object")).boxed(),
        };

        io.add_async_method("personal_newAccount", create_callback);
    }

    let storage = match base_path {
        Some(p) => Storages::new(p),
        None => Storages::default(),
    };

    if storage.init().is_err() {
        panic!("Unable to initialize storage");
    }

    let chain = ChainStorage::new(&storage, "default".to_string());

    if chain.init().is_err() {
        panic!("Unable to initialize chain");
    }

    let dir = chain
        .get_path("contracts".to_string())
        .expect("Expect directory for contracts");

    let contracts = Arc::new(Contracts::new(dir));

    {
        let contracts = contracts.clone();

        io.add_async_method("emerald_contracts",
                            move |_| futures::finished(Value::Array(contracts.list())).boxed());
    }

    {
        let contracts = contracts.clone();

        io.add_async_method("emerald_addContract", move |p| match p {
            Params::Array(ref vec) => {
                match contracts.add(&vec[0]) {
                    Ok(_) => futures::finished(Value::Bool(true)).boxed(),
                    Err(_) => futures::failed(JsonRpcError::new(ErrorCode::InternalError)).boxed(),
                }
            }
            _ => futures::failed(JsonRpcError::new(ErrorCode::InvalidParams)).boxed(),
        });
    }

    let server = ServerBuilder::new(io)
        .cors(DomainsValidation::AllowOnly(vec![cors::AccessControlAllowOrigin::Any,
                                                cors::AccessControlAllowOrigin::Null]))
        .start_http(addr)
        .expect("Expect to build HTTP RPC server");

    if log_enabled!(LogLevel::Info) {
        info!("Connector started on http://{}", server.address());
    }

    server.wait().expect("Expect to start HTTP RPC server");
}
