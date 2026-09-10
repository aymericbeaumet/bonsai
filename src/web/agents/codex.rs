use crate::web::integrations::now;
use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::time::{Duration, Instant};

pub(super) struct ChildPipe {
    child: Child,
    input: ChildStdin,
    output: ChildStdout,
}

impl ChildPipe {
    fn spawn(args: &[&str]) -> Result<Self> {
        Self::spawn_command(Command::new("codex").args(args))
    }

    fn spawn_command(command: &mut Command) -> Result<Self> {
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()?;
        Ok(Self {
            input: child.stdin.take().context("missing Codex input")?,
            output: child.stdout.take().context("missing Codex output")?,
            child,
        })
    }
}

impl Drop for ChildPipe {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Read for ChildPipe {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        #[cfg(unix)]
        {
            use std::os::fd::AsRawFd;
            let mut descriptor = libc::pollfd {
                fd: self.output.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            // poll only observes the owned pipe descriptor; its storage remains valid for the call.
            let ready = unsafe { libc::poll(&mut descriptor, 1, 3000) };
            if ready == 0 {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "Codex response timed out",
                ));
            }
            if ready < 0 {
                return Err(std::io::Error::last_os_error());
            }
        }
        self.output.read(buffer)
    }
}

impl Write for ChildPipe {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        #[cfg(unix)]
        {
            use std::os::fd::AsRawFd;
            let mut descriptor = libc::pollfd {
                fd: self.input.as_raw_fd(),
                events: libc::POLLOUT,
                revents: 0,
            };
            // The owned pipe remains open while polling; small writes avoid filling it mid-call.
            let ready = unsafe { libc::poll(&mut descriptor, 1, 3000) };
            if ready == 0 {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "Codex input timed out",
                ));
            }
            if ready < 0 {
                return Err(std::io::Error::last_os_error());
            }
        }
        self.input.write(&buffer[..buffer.len().min(512)])
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.input.flush()
    }
}

pub(super) struct CodexClient<S = ChildPipe> {
    socket: tungstenite::WebSocket<S>,
    sequence: u64,
}

impl CodexClient {
    pub(super) fn connect() -> Result<Self> {
        let pipe = ChildPipe::spawn(&["app-server", "proxy"])?;
        Self::from_stream(pipe)
    }
}

impl<S: Read + Write> CodexClient<S> {
    fn from_stream(stream: S) -> Result<Self> {
        let config = tungstenite::protocol::WebSocketConfig::default()
            .max_message_size(Some(4 * 1024 * 1024))
            .max_frame_size(Some(4 * 1024 * 1024));
        let (socket, _) =
            tungstenite::client::client_with_config("ws://localhost/", stream, Some(config))
                .map_err(|_| anyhow::anyhow!("cannot connect to existing Codex daemon"))?;
        let mut client = Self {
            socket,
            sequence: 0,
        };
        client.request("initialize", json!({"clientInfo":{"name":"bonsai","version":env!("CARGO_PKG_VERSION")},"capabilities":{"experimentalApi":true}}))?;
        client.socket.send(tungstenite::Message::Text(
            json!({"method":"initialized"}).to_string().into(),
        ))?;
        Ok(client)
    }

    pub(super) fn request(&mut self, method: &str, params: Value) -> Result<Value> {
        self.sequence += 1;
        let id = self.sequence;
        self.socket.send(tungstenite::Message::Text(
            json!({"id":id,"method":method,"params":params})
                .to_string()
                .into(),
        ))?;
        let started = Instant::now();
        while started.elapsed() < Duration::from_secs(4) {
            let message = self.socket.read()?;
            if let tungstenite::Message::Text(text) = message {
                let response: Value = serde_json::from_str(&text)?;
                if response["id"] == id {
                    if let Some(error) = response.get("error") {
                        bail!(
                            "Codex rejected {method}: {}",
                            error["message"].as_str().unwrap_or("protocol error")
                        );
                    }
                    return Ok(response["result"].clone());
                }
            }
        }
        bail!("Codex request timed out")
    }
}

pub(super) fn codex_snapshot(include_quotas: bool) -> Result<(Vec<Value>, Vec<Value>)> {
    let mut client = CodexClient::connect()?;
    snapshot_with_client(&mut client, include_quotas)
}

fn snapshot_with_client<S: Read + Write>(
    client: &mut CodexClient<S>,
    include_quotas: bool,
) -> Result<(Vec<Value>, Vec<Value>)> {
    snapshot_with_budget(client, include_quotas, Duration::from_secs(4))
}

struct SnapshotClient<'a, S> {
    client: &'a mut CodexClient<S>,
    deadline: Instant,
}

impl<S: Read + Write> SnapshotClient<'_, S> {
    fn check_deadline(&self) -> Result<()> {
        ensure!(
            Instant::now() < self.deadline,
            "Codex snapshot deadline exceeded"
        );
        Ok(())
    }

    fn request(&mut self, method: &str, params: Value) -> Result<Value> {
        self.check_deadline()?;
        let result = self.client.request(method, params);
        self.check_deadline()?;
        result
    }
}

fn snapshot_with_budget<S: Read + Write>(
    client: &mut CodexClient<S>,
    include_quotas: bool,
    budget: Duration,
) -> Result<(Vec<Value>, Vec<Value>)> {
    let mut client = SnapshotClient {
        client,
        deadline: Instant::now() + budget,
    };
    let mut agents = Vec::new();
    let loaded = client.request("thread/loaded/list", json!({}))?;
    let loaded_ids: std::collections::BTreeSet<&str> = loaded["data"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect();
    let mut listed_ids = std::collections::BTreeSet::new();
    let mut cursor = Value::Null;
    for _ in 0..10 {
        let result = client.request("thread/list", json!({"limit":100,"cursor":cursor,"sortKey":"updated_at",
            "useStateDbOnly":true,"sourceKinds":["cli","vscode","appServer","exec","subAgent","subAgentReview","subAgentCompact","subAgentThreadSpawn","subAgentOther","unknown"]}))?;
        for thread in result["data"].as_array().into_iter().flatten() {
            if let Some(mut agent) = normalize_codex(thread, now()) {
                if let Some(id) = thread["id"].as_str()
                    && loaded_ids.contains(id)
                    && let Some(current) = loaded_agent(&mut client, id)
                {
                    agent = current;
                }
                if let Some(id) = thread["id"].as_str() {
                    listed_ids.insert(id.to_owned());
                }
                agents.push(agent);
            }
        }
        cursor = result["nextCursor"].clone();
        if cursor.is_null() {
            break;
        }
    }
    // Runtime-only children can be loaded before their metadata enters the state database.
    for id in loaded_ids {
        if !listed_ids.contains(id)
            && let Some(agent) = loaded_agent(&mut client, id)
        {
            agents.push(agent);
        }
    }
    let quotas = if include_quotas {
        let account = client
            .request("account/read", json!({"refreshToken":false}))
            .unwrap_or(Value::Null);
        let limits = client
            .request("account/rateLimits/read", json!({}))
            .unwrap_or(Value::Null);
        normalize_codex_quotas(&limits, &account, now())
    } else {
        Vec::new()
    };
    client.check_deadline()?;
    Ok((agents, quotas))
}

fn loaded_agent<S: Read + Write>(client: &mut SnapshotClient<'_, S>, id: &str) -> Option<Value> {
    let detail = client
        .request("thread/read", json!({"threadId":id,"includeTurns":false}))
        .ok()?;
    let mut agent = normalize_codex(&detail["thread"], now())?;
    if (agent["state"] == "running" || agent["state"] == "waiting")
        // Recent turn metadata contains IDs/status only; never request message bodies.
        && let Ok(turns) = client.request("thread/turns/list",json!({"threadId":id,"limit":1,"sortDirection":"desc","itemsView":"notLoaded"}))
        && let Some(turn) = turns["data"].as_array().into_iter().flatten().find(|turn|turn["status"] == "inProgress")
    {
        agent["turnId"] = turn["id"].clone();
    }
    Some(agent)
}

pub(super) fn normalize_codex(thread: &Value, observed: u64) -> Option<Value> {
    let id = thread["id"].as_str()?;
    let status = thread["status"]["type"].as_str().unwrap_or("notLoaded");
    let flags = thread["status"]["activeFlags"].as_array();
    let waiting = flags.is_some_and(|flags| {
        flags
            .iter()
            .any(|flag| flag == "waitingOnApproval" || flag == "waitingOnUserInput")
    });
    let state = match status {
        "active" if waiting => "waiting",
        "active" => "running",
        "idle" => "idle",
        "systemError" => "failed",
        _ => "unknown",
    };
    let parent = thread["source"]["subAgent"]["thread_spawn"]["parent_thread_id"]
        .as_str()
        .or_else(|| thread["source"]["subAgent"]["threadSpawn"]["parentThreadId"].as_str());
    Some(
        json!({"id":format!("codex:{id}"),"provider":"codex","sessionId":id,"parentId":parent.map(|id|format!("codex:{id}")),
        "cwd":thread["cwd"],"worktreePath":null,"title":thread["name"].as_str().unwrap_or("Codex"),"model":thread["model"],
        "state":state,"waitingReason":if waiting {Some(if flags.is_some_and(|flags|flags.iter().any(|flag|flag == "waitingOnApproval")) {"Permission required"} else {"Input needed"})} else {None},
        "updatedAt":thread["updatedAt"],"observedAt":observed,"live":status != "notLoaded","stale":false,
        "target":null,"capabilities":[],"nativeRuntime":status != "notLoaded",
        "source":if status == "notLoaded" {"history"} else {"native"},"completionObserved":false}),
    )
}

pub(super) fn codex_account_quotas() -> Result<Vec<Value>> {
    account_quotas_from_stream(ChildPipe::spawn(&["app-server", "--listen", "stdio://"])?)
}

fn account_quotas_from_stream<S: Read + Write>(stream: S) -> Result<Vec<Value>> {
    let mut pipe = BufReader::new(stream);
    let request =
        |pipe: &mut BufReader<S>, id: u64, method: &str, params: Value| -> Result<Value> {
            writeln!(
                pipe.get_mut(),
                "{}",
                json!({"id":id,"method":method,"params":params})
            )?;
            pipe.get_mut().flush()?;
            let start = Instant::now();
            while start.elapsed() < Duration::from_secs(4) {
                let mut line = String::new();
                ensure!(
                    pipe.by_ref()
                        .take(4 * 1024 * 1024 + 1)
                        .read_line(&mut line)?
                        > 0,
                    "Codex account reader closed"
                );
                ensure!(
                    line.len() < 4 * 1024 * 1024,
                    "Codex response exceeded four MiB"
                );
                let response: Value = serde_json::from_str(&line)?;
                if response["id"] == id {
                    ensure!(
                        response.get("error").is_none(),
                        "Codex account request failed"
                    );
                    return Ok(response["result"].clone());
                }
            }
            bail!("Codex account request timed out")
        };
    request(
        &mut pipe,
        1,
        "initialize",
        json!({"clientInfo":{"name":"bonsai","version":env!("CARGO_PKG_VERSION")}}),
    )?;
    writeln!(pipe.get_mut(), "{}", json!({"method":"initialized"}))?;
    pipe.get_mut().flush()?;
    // Account RPCs need initialization, but never load or resume a thread.
    let account = request(&mut pipe, 2, "account/read", json!({"refreshToken":false}))?;
    let limits = request(&mut pipe, 3, "account/rateLimits/read", json!({}))?;
    Ok(normalize_codex_quotas(&limits, &account, now()))
}

pub(super) fn normalize_codex_quotas(limits: &Value, account: &Value, observed: u64) -> Vec<Value> {
    let mut quotas = Vec::new();
    let buckets: Vec<(&str, &Value)> =
        if let Some(buckets) = limits["rateLimitsByLimitId"].as_object() {
            buckets
                .iter()
                .map(|(id, bucket)| (id.as_str(), bucket))
                .collect()
        } else {
            vec![("codex", &limits["rateLimits"])]
        };
    for (id, bucket) in buckets {
        for window in ["primary", "secondary"] {
            if let Some(used) = bucket[window]["usedPercent"].as_f64() {
                let minutes = bucket[window]["windowDurationMins"].as_u64().unwrap_or(0);
                quotas.push(json!({"id":format!("codex:{id}:{window}"),"provider":"codex",
                    "accountLabel":account["account"]["email"],"label":format!("{} · {}", bucket["limitName"].as_str().unwrap_or(id),
                        if minutes >= 1440 {format!("{} days",minutes/1440)} else if minutes >= 60 {format!("{} hours",minutes/60)} else {format!("{minutes} minutes")}),
                    "usedPercent":used,"resetsAt":bucket[window]["resetsAt"],"observedAt":observed,"stale":false}));
            }
        }
    }
    quotas
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::net::UnixStream;

    fn pair() -> (UnixStream, UnixStream) {
        let (client, server) = UnixStream::pair().unwrap();
        client
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        client
            .set_write_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        server
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        server
            .set_write_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        (client, server)
    }

    #[test]
    fn existing_socket_protocol_paginates_and_keeps_exact_active_turn_for_control() {
        let (client, server) = pair();
        let server = std::thread::spawn(move || {
            let mut socket = tungstenite::accept(server).unwrap();
            let mut methods = Vec::new();
            loop {
                let message = socket.read().unwrap();
                let request: Value = serde_json::from_str(message.to_text().unwrap()).unwrap();
                let method = request["method"].as_str().unwrap();
                methods.push(method.to_owned());
                let result = match method {
                    "initialize" => json!({}),
                    "initialized" => {
                        assert!(request.get("id").is_none());
                        continue;
                    }
                    "thread/loaded/list" => json!({"data":["active","fresh-child"]}),
                    "thread/list" if request["params"]["cursor"].is_null() => {
                        assert_eq!(request["params"]["useStateDbOnly"], true);
                        json!({"data":[{"id":"active","cwd":"/work","status":{"type":"active"}}],"nextCursor":"next"})
                    }
                    "thread/list" => {
                        assert_eq!(request["params"]["cursor"], "next");
                        json!({"data":[{"id":"old","status":{"type":"notLoaded"}}],"nextCursor":null})
                    }
                    "thread/read" => {
                        assert_eq!(request["params"]["includeTurns"], false);
                        json!({"thread":{"id":request["params"]["threadId"],"cwd":"/work","status":{"type":"active"}}})
                    }
                    "thread/turns/list" => {
                        assert_eq!(request["params"]["itemsView"], "notLoaded");
                        json!({"data":[{"id":"turn-7","status":"inProgress"}]})
                    }
                    "turn/interrupt" => {
                        assert_eq!(
                            request["params"],
                            json!({"threadId":"active","turnId":"turn-7"})
                        );
                        socket
                            .send(tungstenite::Message::Text(
                                json!({"id":request["id"],"result":{}}).to_string().into(),
                            ))
                            .unwrap();
                        break;
                    }
                    unexpected => panic!("unexpected protocol call {unexpected}"),
                };
                socket
                    .send(tungstenite::Message::Text(
                        json!({"id":request["id"],"result":result})
                            .to_string()
                            .into(),
                    ))
                    .unwrap();
            }
            methods
        });
        let mut client = CodexClient::from_stream(client).unwrap();
        let (agents, quotas) = snapshot_with_client(&mut client, false).unwrap();
        assert_eq!(agents.len(), 3);
        assert_eq!(agents[0]["turnId"], "turn-7");
        assert_eq!(agents[1]["live"], false);
        assert_eq!(agents[2]["id"], "codex:fresh-child");
        assert_eq!(agents[2]["live"], true);
        assert!(quotas.is_empty());
        client
            .request(
                "turn/interrupt",
                json!({"threadId":"active","turnId":agents[0]["turnId"]}),
            )
            .unwrap();
        let methods = server.join().unwrap();
        assert_eq!(methods[0..2], ["initialize", "initialized"]);
        assert!(!methods.iter().any(|method| method.starts_with("account/")));
    }

    #[test]
    fn quota_only_stdio_reader_initializes_and_never_reads_thread_state() {
        let (client, server) = pair();
        let server = std::thread::spawn(move || {
            let mut stream = BufReader::new(server);
            for expected in [
                "initialize",
                "initialized",
                "account/read",
                "account/rateLimits/read",
            ] {
                let mut line = String::new();
                stream.read_line(&mut line).unwrap();
                let request: Value = serde_json::from_str(&line).unwrap();
                assert_eq!(request["method"], expected);
                if expected == "initialized" {
                    assert!(request.get("id").is_none());
                    continue;
                }
                let result = match expected {
                    "account/read" => {
                        assert_eq!(request["params"]["refreshToken"], false);
                        json!({"account":{"type":"chatgpt","email":"test@example.invalid"}})
                    }
                    "account/rateLimits/read" => {
                        json!({"rateLimits":{"primary":{"usedPercent":32,"resetsAt":999,"windowDurationMins":300}}})
                    }
                    _ => json!({}),
                };
                writeln!(
                    stream.get_mut(),
                    "{}",
                    json!({"id":request["id"],"result":result})
                )
                .unwrap();
                stream.get_mut().flush().unwrap();
            }
        });
        let quotas = account_quotas_from_stream(client).unwrap();
        assert_eq!(quotas.len(), 1);
        assert_eq!(quotas[0]["usedPercent"], 32.0);
        assert_eq!(quotas[0]["resetsAt"], 999);
        server.join().unwrap();
    }

    #[test]
    fn snapshot_deadline_stops_further_requests_and_rejects_partial_observations() {
        let (client, server) = pair();
        let server = std::thread::spawn(move || {
            let mut socket = tungstenite::accept(server).unwrap();
            for expected in [
                "initialize",
                "initialized",
                "thread/loaded/list",
                "thread/list",
                "thread/read",
            ] {
                let message = socket.read().unwrap();
                let request: Value = serde_json::from_str(message.to_text().unwrap()).unwrap();
                assert_eq!(request["method"], expected);
                let result = match expected {
                    "initialized" => continue,
                    "thread/loaded/list" => json!({"data":["active"]}),
                    "thread/list" => json!({"data":[{"id":"active","status":{"type":"active"}}]}),
                    "thread/read" => {
                        std::thread::sleep(Duration::from_millis(300));
                        json!({"thread":{"id":"active","status":{"type":"active"}}})
                    }
                    _ => json!({}),
                };
                socket
                    .send(tungstenite::Message::Text(
                        json!({"id":request["id"],"result":result})
                            .to_string()
                            .into(),
                    ))
                    .unwrap();
            }
            assert!(socket.read().is_err(), "expired snapshot sent another RPC");
        });
        let mut client = CodexClient::from_stream(client).unwrap();
        let result = snapshot_with_budget(&mut client, true, Duration::from_millis(250));
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("snapshot deadline")
        );
        drop(client);
        server.join().unwrap();
    }

    #[test]
    fn protocol_errors_are_returned_without_treating_them_as_success() {
        let (client, server) = pair();
        let server = std::thread::spawn(move || {
            let mut socket = tungstenite::accept(server).unwrap();
            let message = socket.read().unwrap();
            let request: Value = serde_json::from_str(message.to_text().unwrap()).unwrap();
            socket
                .send(tungstenite::Message::Text(
                    json!({"id":request["id"],"error":{"message":"unsupported client"}})
                        .to_string()
                        .into(),
                ))
                .unwrap();
        });
        assert!(
            CodexClient::from_stream(client)
                .err()
                .unwrap()
                .to_string()
                .contains("unsupported client")
        );
        server.join().unwrap();
    }

    #[test]
    fn dropping_provider_transport_reaps_its_process() {
        let pipe =
            ChildPipe::spawn_command(Command::new("sh").args(["-c", "exec sleep 30"])).unwrap();
        let pid = pipe.child.id();
        drop(pipe);
        // kill with signal zero only probes existence; Drop already terminated the child.
        assert_eq!(unsafe { libc::kill(pid as libc::pid_t, 0) }, -1);
    }
}
