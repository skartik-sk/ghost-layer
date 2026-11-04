//! `ghost mcp`: Model Context Protocol server (stdio) so AI agents can use Ghost Layer.
//!
//! Read-only tools by default: ghost_status, ghost_verify, ghost_ip, ghost_doctor, ghost_report,
//! ghost_explain. `ghost_pay` exists only when started with `--allow-pay --max-amount <TAZ>
//! --max-per-hour <n> --wallet <dir> --identity <file>`; caps are enforced here, testnet only,
//! and every call is logged to ~/.ghost-layer/mcp-calls.jsonl. The agent can never choose the
//! wallet or identity, and Ghost never reads keys (zcash-devtool does, as a subprocess).
//!
//! Each tool runs `ghost --json <command>` as a subprocess and returns its JSON: the same
//! stable contract as the CLI (docs/json-schema.md), and nothing can corrupt the stdio channel.

use std::io::Write;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use rmcp::{
    ErrorData as McpError, ServerHandler, ServiceExt,
    handler::server::{tool::ToolRouter, wrapper::Parameters},
    model::{CallToolResult, ContentBlock, ServerCapabilities, ServerConfig},
    schemars, tool, tool_handler, tool_router,
    transport::stdio,
};
use serde::Deserialize;
use serde_json::json;

use crate::{Fail, data_dir};

#[derive(clap::Args, Clone)]
pub(crate) struct McpArgs {
    /// Enable the ghost_pay tool (off by default)
    #[arg(long)]
    allow_pay: bool,
    /// Max TAZ per ghost_pay call (required with --allow-pay)
    #[arg(long)]
    max_amount: Option<String>,
    /// Max ghost_pay calls per rolling hour (required with --allow-pay)
    #[arg(long)]
    max_per_hour: Option<u32>,
    /// Wallet ghost_pay may use (fixed at startup; the agent can't choose another)
    #[arg(long)]
    wallet: Option<PathBuf>,
    /// Its age identity file (path only; passed to zcash-devtool)
    #[arg(long)]
    identity: Option<PathBuf>,
    /// zcash-devtool binary for ghost_pay
    #[arg(long)]
    devtool: Option<PathBuf>,
    /// Make ghost_pay / ghost_verify / ghost_ip / ghost_doctor use the Nym clients
    #[arg(long)]
    nym: bool,
}

#[derive(Clone)]
struct PayPolicy {
    max_zat: u64,
    max_per_hour: u32,
    wallet: PathBuf,
    identity: PathBuf,
    devtool: Option<PathBuf>,
    calls: Arc<Mutex<Vec<Instant>>>,
}

#[derive(Clone)]
pub(crate) struct GhostMcp {
    nym: bool,
    pay: Option<PayPolicy>,
    tool_router: ToolRouter<Self>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct VerifyArgs {
    /// Transaction id (64 hex chars, as shown by explorers)
    pub txid: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ReportArgs {
    /// Upstream event log to analyse (default: ~/.ghost-layer/upstream-events.jsonl)
    pub events: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct PayToolArgs {
    /// Testnet recipient address (utest1…, ztestsapling1…, tm…)
    pub to: String,
    /// Amount in TAZ, e.g. "0.001"
    pub amount: String,
    /// Optional memo
    pub memo: Option<String>,
}

fn log_call(tool: &str, args: serde_json::Value, outcome: &str) {
    let line = json!({"ts_ms": ghost_core::broadcast::now_ms(), "tool": tool, "args": args, "outcome": outcome});
    eprintln!("ghost mcp: {line}");
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(data_dir().join("mcp-calls.jsonl")) {
        let _ = writeln!(f, "{line}");
    }
}

/// Runs `ghost --json <args>` and returns its JSON output as tool content.
async fn run_ghost(tool: &str, args: Vec<String>, log_args: serde_json::Value, timeout: Duration) -> CallToolResult {
    let exe = match std::env::current_exe() {
        Ok(e) => e,
        Err(e) => return CallToolResult::error(vec![ContentBlock::text(format!("cannot find ghost binary: {e}"))]),
    };
    let mut cmd = tokio::process::Command::new(exe);
    cmd.arg("--json").arg("--quiet").args(&args).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).kill_on_drop(true);
    let out = match tokio::time::timeout(timeout, cmd.output()).await {
        Ok(Ok(o)) => o,
        Ok(Err(e)) => return CallToolResult::error(vec![ContentBlock::text(format!("failed to run ghost: {e}"))]),
        Err(_) => {
            log_call(tool, log_args, "timeout");
            return CallToolResult::error(vec![ContentBlock::text(format!("timed out after {} s", timeout.as_secs()))]);
        }
    };
    let stdout = String::from_utf8_lossy(&out.stdout).trim().to_string();
    let code = out.status.code().unwrap_or(-1);
    log_call(tool, log_args, &format!("exit {code}"));
    let body = if stdout.is_empty() { json!({"ok": false, "exit_code": code}).to_string() } else { stdout };
    if code == 0 {
        CallToolResult::success(vec![ContentBlock::text(body)])
    } else {
        CallToolResult::error(vec![ContentBlock::text(body)])
    }
}

#[tool_router]
impl GhostMcp {
    fn new(nym: bool, pay: Option<PayPolicy>) -> Self {
        let mut s = Self { nym, pay, tool_router: Self::tool_router() };
        if s.pay.is_none() {
            s.tool_router.remove_route("ghost_pay");
        }
        s
    }

    fn nym_flag(&self) -> Vec<String> {
        if self.nym { vec!["--nym".into()] } else { vec![] }
    }

    #[tool(description = "Ghost Layer broadcast queue: each payment's status (pending/accepted/rejected/failed), random delay, fresh connection id, transport, explorer links. Read-only.")]
    async fn ghost_status(&self) -> CallToolResult {
        run_ghost("ghost_status", vec!["status".into()], json!({}), Duration::from_secs(30)).await
    }

    #[tool(description = "Check whether a Zcash testnet transaction is mined: state, height, confirmations, explorer links. Read-only.")]
    async fn ghost_verify(&self, Parameters(VerifyArgs { txid }): Parameters<VerifyArgs>) -> CallToolResult {
        if txid.len() != 64 || !txid.chars().all(|c| c.is_ascii_hexdigit()) {
            return CallToolResult::error(vec![ContentBlock::text("txid must be 64 hex characters")]);
        }
        let mut args = vec!["verify".into()];
        args.extend(self.nym_flag());
        args.push(txid.clone());
        run_ghost("ghost_verify", args, json!({"txid": txid}), Duration::from_secs(180)).await
    }

    #[tool(description = "Measure which IP the light-wallet server sees on the SYNC and BROADCAST paths (your own IP is masked). Read-only.")]
    async fn ghost_ip(&self) -> CallToolResult {
        let mut args = vec!["ip".into()];
        args.extend(self.nym_flag());
        run_ghost("ghost_ip", args, json!({}), Duration::from_secs(240)).await
    }

    #[tool(description = "Health check: upstream reachable, chain = test, NU7 consensus branch, tip height, TLS, port, storage, Nym clients. Read-only.")]
    async fn ghost_doctor(&self) -> CallToolResult {
        let mut args = vec!["doctor".into(), "--listen".into(), "127.0.0.1:19067".into()];
        args.extend(self.nym_flag());
        run_ghost("ghost_doctor", args, json!({}), Duration::from_secs(300)).await
    }

    #[tool(description = "Attacker view: run the linkage heuristics (same-connection, sync-then-send, range fingerprint, same IP) over Ghost Layer's upstream event log and return per-payment results and the overall linkage rate. Read-only.")]
    async fn ghost_report(&self, Parameters(ReportArgs { events }): Parameters<ReportArgs>) -> CallToolResult {
        let path = events.map(PathBuf::from).unwrap_or_else(|| data_dir().join("upstream-events.jsonl"));
        if !path.exists() {
            return CallToolResult::error(vec![ContentBlock::text(format!(
                "no event log at {} yet: run `ghost up` and make a payment first", path.display()))]);
        }
        run_ghost("ghost_report", vec!["attack".into(), "--events".into(), path.display().to_string()],
            json!({"events": path}), Duration::from_secs(60)).await
    }

    #[tool(description = "Plain-language explanation of the three network leaks (IP, sync-then-send timing, block-range fingerprint) and what Ghost Layer does about each. Read-only.")]
    async fn ghost_explain(&self) -> CallToolResult {
        run_ghost("ghost_explain", vec!["explain".into()], json!({}), Duration::from_secs(10)).await
    }

    #[tool(description = "Send a private TESTNET payment through Ghost Layer (sync via gateway, random delay, fresh broadcast connection, waits until mined). Only available when the server was started with --allow-pay; amount and rate caps are enforced.")]
    async fn ghost_pay(&self, Parameters(PayToolArgs { to, amount, memo }): Parameters<PayToolArgs>) -> Result<CallToolResult, McpError> {
        let Some(p) = &self.pay else {
            return Ok(CallToolResult::error(vec![ContentBlock::text("ghost_pay is disabled (start with --allow-pay)")]));
        };
        let refuse = |why: String| {
            log_call("ghost_pay", json!({"to": to, "amount": amount}), &format!("refused: {why}"));
            Ok(CallToolResult::error(vec![ContentBlock::text(format!("refused: {why}"))]))
        };
        if !(to.starts_with("utest1") || to.starts_with("ztestsapling1") || to.starts_with("tm")) {
            return refuse("testnet addresses only (utest1…, ztestsapling1…, tm…)".into());
        }
        let zat = match crate::pay::taz_to_zat(&amount) {
            Ok(z) => z,
            Err(f) => return refuse(f.msg),
        };
        if zat > p.max_zat {
            return refuse(format!("amount {amount} TAZ exceeds the cap of {} zat", p.max_zat));
        }
        {
            let mut calls = p.calls.lock().unwrap();
            calls.retain(|t| t.elapsed() < Duration::from_secs(3600));
            if calls.len() as u32 >= p.max_per_hour {
                return refuse(format!("rate cap reached ({} payments per hour)", p.max_per_hour));
            }
            calls.push(Instant::now());
        }
        let mut args = vec!["pay".into(), "--wallet".into(), p.wallet.display().to_string(), "--identity".into(),
            p.identity.display().to_string(), "--to".into(), to.clone(), "--amount".into(), amount.clone()];
        if let Some(m) = &memo {
            args.extend(["--memo".into(), m.clone()]);
        }
        if let Some(d) = &p.devtool {
            args.extend(["--devtool".into(), d.display().to_string()]);
        }
        args.extend(self.nym_flag());
        Ok(run_ghost("ghost_pay", args, json!({"to": to, "amount": amount, "memo": memo.is_some()}), Duration::from_secs(1200)).await)
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for GhostMcp {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(rmcp::model::Implementation::new("ghost-layer", env!("CARGO_PKG_VERSION")))
            .with_instructions(
                "Ghost Layer: unlinks Zcash testnet payments from wallet sync (split broadcast, random delay, \
                 grid-aligned sync, Nym). Tools return the CLI's --json output (docs/json-schema.md). \
                 Testnet only; NU7 is active and new shielded funds use the Ironwood pool.",
            )
    }
}

pub(crate) async fn mcp(a: McpArgs) -> Result<u8, Fail> {
    let pay = if a.allow_pay {
        let (Some(max), Some(per_hour), Some(wallet), Some(identity)) = (a.max_amount.clone(), a.max_per_hour, a.wallet.clone(), a.identity.clone())
        else {
            return Err(Fail::usage("--allow-pay needs --max-amount, --max-per-hour, --wallet and --identity",
                "e.g. ghost mcp --allow-pay --max-amount 0.001 --max-per-hour 3 --wallet ./w --identity ./w.age"));
        };
        Some(PayPolicy { max_zat: crate::pay::taz_to_zat(&max)?, max_per_hour: per_hour, wallet, identity, devtool: a.devtool.clone(),
            calls: Arc::new(Mutex::new(Vec::new())) })
    } else {
        None
    };
    eprintln!("ghost mcp: stdio server ready (ghost_pay {})", if pay.is_some() { "ENABLED with caps" } else { "disabled" });
    let service = GhostMcp::new(a.nym, pay)
        .serve(stdio())
        .await
        .map_err(|e| Fail::runtime(format!("mcp: {e}"), "run from an MCP client (stdio)"))?;
    service.waiting().await.map_err(|e| Fail::runtime(format!("mcp: {e}"), "restart the MCP client"))?;
    Ok(0)
}
