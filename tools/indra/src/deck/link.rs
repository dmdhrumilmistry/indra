//! The agent link: nodes that dial in to the hub, instead of being reached
//! over SSH.
//!
//! ```text
//!  node: indra node connect https://hub  --(WebSocket, outbound)-->  hub: /agent/connect
//!        jobs run here                    <==(WebRTC data channel)==>  hub: the console
//! ```
//!
//! - **Enrol once.** The console makes a one-time code for a node name; the
//!   node connects with it and receives a key, kept in the hub's encrypted
//!   secret store and in an owner-only file on the node. Later connections
//!   present the key.
//! - **Signal and relay over the WebSocket.** It carries heartbeats, WebRTC
//!   offers, answers and ICE candidates, and job traffic whenever there is
//!   no direct path.
//! - **Go direct when possible.** With the `webrtc` feature, hub and node
//!   open a peer-to-peer data channel (DTLS-encrypted) and job traffic moves
//!   onto it; frames too large for it, and everything when it is down, take
//!   the WebSocket.
//! - **Survive drops.** Both sides send heartbeats and declare the link dead
//!   after a silence. The node reconnects with exponential backoff and
//!   jitter. Job output carries sequence numbers; the node keeps what the hub
//!   has not acknowledged and sends it again after a reconnect, and the hub
//!   drops what it has already seen, so each line arrives exactly once.
//! - **No orphans.** A hub fails a node's jobs if the node stays away past a
//!   grace period; a node stops its jobs if the hub stays away longer, or
//!   comes back as a different hub process.
//!
//! Frames are JSON text. Hub to node: welcome, ping, ack, job, in, close,
//! stop, rtc, enrolled. Node to hub: hello, pong, ev, rtc.

use crate::deck::nodes::{Job, JobInput};
use serde_json::{json, Value};
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::mpsc;

pub type Result<T> = std::result::Result<T, String>;

/// Heartbeat interval, and the silence after which a link is dead.
pub const HEARTBEAT: Duration = Duration::from_secs(5);
pub const DEAD_AFTER: u64 = 15;
/// How long the hub keeps a node's jobs while it is away.
pub const HUB_GRACE: u64 = 120;
/// How long a node keeps running jobs while the hub is away.
pub const NODE_GRACE: u64 = 600;
/// Frames larger than this go over the WebSocket (data channel messages are small).
const DC_MAX: usize = 60_000;
/// Events a node keeps per job until the hub acknowledges them.
const BUFFER: usize = 20_000;

fn now() -> u64 {
    crate::deck::runs::now()
}

pub(crate) fn random_token(bytes: usize) -> String {
    let mut b = vec![0u8; bytes];
    let _ = getrandom::fill(&mut b);
    b.iter().map(|x| format!("{:02x}", x)).collect()
}

/// The secret-store entry that holds a node's key.
pub fn key_name(node: &str) -> String {
    format!("node.{}", node)
}

// ---- WebRTC ------------------------------------------------------------------------

#[cfg(feature = "webrtc")]
pub mod rtc {
    use super::*;
    use webrtc::api::APIBuilder;
    use webrtc::data_channel::data_channel_message::DataChannelMessage;
    use webrtc::data_channel::RTCDataChannel;
    use webrtc::ice_transport::ice_candidate::{RTCIceCandidate, RTCIceCandidateInit};
    use webrtc::ice_transport::ice_server::RTCIceServer;
    use webrtc::peer_connection::configuration::RTCConfiguration;
    use webrtc::peer_connection::peer_connection_state::RTCPeerConnectionState;
    use webrtc::peer_connection::sdp::sdp_type::RTCSdpType;
    use webrtc::peer_connection::sdp::session_description::RTCSessionDescription;
    use webrtc::peer_connection::RTCPeerConnection;

    /// One side of a peer-to-peer data channel.
    pub struct Peer {
        pc: Arc<RTCPeerConnection>,
        out: mpsc::UnboundedSender<String>,
        open: Arc<AtomicBool>,
        pending: Mutex<Vec<RTCIceCandidateInit>>,
        remote_set: AtomicBool,
        signal: mpsc::UnboundedSender<String>,
    }

    fn err(e: impl std::fmt::Display) -> String {
        format!("webrtc: {}", e)
    }

    impl Peer {
        /// `signal` takes frames for the other side (over the WebSocket);
        /// `inbound` receives frames that arrive over the data channel.
        pub async fn new(
            ice: &[super::IceServer],
            signal: mpsc::UnboundedSender<String>,
            inbound: mpsc::UnboundedSender<String>,
            offerer: bool,
        ) -> Result<Arc<Peer>> {
            let api = APIBuilder::new().build();
            let cfg = RTCConfiguration {
                ice_servers: ice
                    .iter()
                    .map(|s| RTCIceServer {
                        urls: s.urls.clone(),
                        username: s.username.clone(),
                        credential: s.credential.clone(),
                    })
                    .collect(),
                ..Default::default()
            };
            let pc = Arc::new(api.new_peer_connection(cfg).await.map_err(err)?);
            let (out, out_rx) = mpsc::unbounded_channel::<String>();
            let peer = Arc::new(Peer {
                pc: pc.clone(),
                out,
                open: Arc::new(AtomicBool::new(false)),
                pending: Mutex::new(Vec::new()),
                remote_set: AtomicBool::new(false),
                signal: signal.clone(),
            });
            let sig = signal.clone();
            pc.on_ice_candidate(Box::new(move |c: Option<RTCIceCandidate>| {
                let sig = sig.clone();
                Box::pin(async move {
                    if let Some(init) = c.and_then(|c| c.to_json().ok()) {
                        let _ = sig.send(json!({ "t": "rtc", "ice": init }).to_string());
                    }
                })
            }));
            let open = peer.open.clone();
            pc.on_peer_connection_state_change(Box::new(move |s: RTCPeerConnectionState| {
                let open = open.clone();
                Box::pin(async move {
                    if matches!(
                        s,
                        RTCPeerConnectionState::Failed
                            | RTCPeerConnectionState::Disconnected
                            | RTCPeerConnectionState::Closed
                    ) {
                        open.store(false, Ordering::SeqCst);
                    }
                })
            }));
            let out_rx = Arc::new(tokio::sync::Mutex::new(Some(out_rx)));
            let wire = {
                let open = peer.open.clone();
                Arc::new(move |dc: Arc<RTCDataChannel>| {
                    let (open2, rx, dc2) = (open.clone(), out_rx.clone(), dc.clone());
                    dc.on_open(Box::new(move || {
                        Box::pin(async move {
                            if let Some(mut rx) = rx.lock().await.take() {
                                open2.store(true, Ordering::SeqCst);
                                tokio::spawn(async move {
                                    while let Some(s) = rx.recv().await {
                                        if dc2.send_text(s).await.is_err() {
                                            break;
                                        }
                                    }
                                });
                            }
                        })
                    }));
                    let open3 = open.clone();
                    dc.on_close(Box::new(move || {
                        let open3 = open3.clone();
                        Box::pin(async move { open3.store(false, Ordering::SeqCst) })
                    }));
                    let inbound = inbound.clone();
                    dc.on_message(Box::new(move |m: DataChannelMessage| {
                        let inbound = inbound.clone();
                        Box::pin(async move {
                            if let Ok(t) = String::from_utf8(m.data.to_vec()) {
                                let _ = inbound.send(t);
                            }
                        })
                    }));
                })
            };
            if offerer {
                let dc = pc.create_data_channel("indra", None).await.map_err(err)?;
                wire(dc);
                let offer = pc.create_offer(None).await.map_err(err)?;
                pc.set_local_description(offer.clone()).await.map_err(err)?;
                let _ = signal.send(json!({ "t": "rtc", "sdp": offer }).to_string());
            } else {
                pc.on_data_channel(Box::new(move |dc: Arc<RTCDataChannel>| {
                    wire(dc);
                    Box::pin(async {})
                }));
            }
            Ok(peer)
        }

        /// An offer, answer or ICE candidate from the other side.
        pub async fn on_signal(&self, v: &Value) -> Result<()> {
            if let Some(sdp) = v.get("sdp") {
                let desc: RTCSessionDescription =
                    serde_json::from_value(sdp.clone()).map_err(err)?;
                let is_offer = desc.sdp_type == RTCSdpType::Offer;
                self.pc.set_remote_description(desc).await.map_err(err)?;
                self.remote_set.store(true, Ordering::SeqCst);
                let queued: Vec<_> = std::mem::take(&mut *self.pending.lock().unwrap());
                for c in queued {
                    let _ = self.pc.add_ice_candidate(c).await;
                }
                if is_offer {
                    let answer = self.pc.create_answer(None).await.map_err(err)?;
                    self.pc
                        .set_local_description(answer.clone())
                        .await
                        .map_err(err)?;
                    let _ = self
                        .signal
                        .send(json!({ "t": "rtc", "sdp": answer }).to_string());
                }
            }
            if let Some(ice) = v.get("ice") {
                let c: RTCIceCandidateInit = serde_json::from_value(ice.clone()).map_err(err)?;
                if self.remote_set.load(Ordering::SeqCst) {
                    self.pc.add_ice_candidate(c).await.map_err(err)?;
                } else {
                    self.pending.lock().unwrap().push(c);
                }
            }
            Ok(())
        }

        pub fn is_open(&self) -> bool {
            self.open.load(Ordering::SeqCst)
        }

        /// Send over the data channel if it is open and the frame fits.
        pub fn send(&self, s: &str) -> bool {
            s.len() <= DC_MAX && self.is_open() && self.out.send(s.to_string()).is_ok()
        }

        pub async fn close(&self) {
            self.open.store(false, Ordering::SeqCst);
            let _ = self.pc.close().await;
        }
    }
}

/// The peer-to-peer path, when this build has it.
#[cfg(feature = "webrtc")]
type PeerSlot = Mutex<Option<Arc<rtc::Peer>>>;
#[cfg(not(feature = "webrtc"))]
type PeerSlot = Mutex<Option<()>>;

// ---- hub side ----------------------------------------------------------------------

struct JobSink {
    lines: mpsc::UnboundedSender<String>,
    seq: u64,
}

/// One node as the hub sees it, across its connections.
pub struct NodeLink {
    pub name: String,
    conn: Mutex<Option<(u64, mpsc::UnboundedSender<String>)>>,
    jobs: Mutex<HashMap<u64, JobSink>>,
    outbox: Mutex<VecDeque<String>>,
    hello: Mutex<Option<Value>>,
    last_seen: AtomicU64,
    since: AtomicU64,
    epoch: AtomicU64,
    peer: PeerSlot,
    /// Requests waiting for the node's answer (`req`/`res` frames).
    reqs: Mutex<HashMap<u64, tokio::sync::oneshot::Sender<Result<Value>>>>,
    next_req: AtomicU64,
}

impl NodeLink {
    fn new(name: &str) -> NodeLink {
        NodeLink {
            name: name.to_string(),
            conn: Mutex::new(None),
            jobs: Mutex::new(HashMap::new()),
            outbox: Mutex::new(VecDeque::new()),
            hello: Mutex::new(None),
            last_seen: AtomicU64::new(0),
            since: AtomicU64::new(0),
            epoch: AtomicU64::new(0),
            peer: Mutex::new(None),
            reqs: Mutex::new(HashMap::new()),
            next_req: AtomicU64::new(1),
        }
    }

    /// Ask the node something (`runs`, `info`) and wait for its answer: what
    /// `indra node runs|info` answer over SSH, for a node that dials in.
    pub async fn request(&self, what: &str, args: Value, secs: u64) -> Result<Value> {
        if !self.connected() {
            return Err(format!("node '{}' is not connected", self.name));
        }
        let id = self.next_req.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.reqs.lock().unwrap().insert(id, tx);
        self.send(json!({ "t": "req", "id": id, "what": what, "args": args }));
        let r = tokio::time::timeout(std::time::Duration::from_secs(secs), rx).await;
        self.reqs.lock().unwrap().remove(&id);
        match r {
            Ok(Ok(r)) => r,
            Ok(Err(_)) => Err(format!("node '{}' went away", self.name)),
            Err(_) => Err(format!(
                "node '{}' did not answer in {}s (an older indra there?)",
                self.name, secs
            )),
        }
    }

    pub fn connected(&self) -> bool {
        self.conn.lock().unwrap().is_some()
    }

    fn direct(&self) -> bool {
        #[cfg(feature = "webrtc")]
        {
            self.peer
                .lock()
                .unwrap()
                .as_ref()
                .map(|p| p.is_open())
                .unwrap_or(false)
        }
        #[cfg(not(feature = "webrtc"))]
        {
            false
        }
    }

    /// Send a frame: the data channel if open, else the WebSocket, else
    /// keep it until the node is back.
    fn send(&self, frame: Value) {
        let s = frame.to_string();
        #[cfg(feature = "webrtc")]
        if let Some(p) = self.peer.lock().unwrap().as_ref() {
            if p.send(&s) {
                return;
            }
        }
        if let Some((_, tx)) = self.conn.lock().unwrap().as_ref() {
            if tx.send(s.clone()).is_ok() {
                return;
            }
        }
        let mut o = self.outbox.lock().unwrap();
        o.push_back(s);
        while o.len() > 10_000 {
            o.pop_front();
        }
    }

    /// What the console shows: connected, the path, and what the node reported.
    pub fn view(&self) -> Value {
        json!({
            "name": self.name, "connected": self.connected(),
            "path": if !self.connected() { "none" } else if self.direct() { "webrtc" } else { "relay" },
            "since": self.since.load(Ordering::SeqCst), "last_seen": self.last_seen.load(Ordering::SeqCst),
            "hello": self.hello.lock().unwrap().clone(), "jobs": self.jobs.lock().unwrap().len(),
        })
    }

    /// The targets the node reported (used when the hub's config lists none).
    pub fn targets(&self) -> Vec<String> {
        self.hello
            .lock()
            .unwrap()
            .as_ref()
            .and_then(|h| h.get("targets").and_then(Value::as_array).cloned())
            .unwrap_or_default()
            .iter()
            .filter_map(|t| t.as_str().map(String::from))
            .collect()
    }

    /// Node facts from its hello, in the shape `indra node info` gives.
    pub fn info(&self) -> Option<Value> {
        self.hello
            .lock()
            .unwrap()
            .as_ref()
            .and_then(|h| h.get("info").cloned())
    }
}

pub struct Links {
    /// This hub process; a node that sees another one stops its old jobs.
    pub hub_id: String,
    nodes: Mutex<HashMap<String, Arc<NodeLink>>>,
    codes: Mutex<HashMap<String, (String, u64)>>,
    next_job: AtomicU64,
    /// STUN/TURN servers for WebRTC (none: direct paths on the same network only).
    pub ice_servers: Mutex<Vec<IceServer>>,
    pub webrtc: AtomicBool,
}

impl Default for Links {
    fn default() -> Self {
        Links {
            hub_id: random_token(8),
            nodes: Mutex::new(HashMap::new()),
            codes: Mutex::new(HashMap::new()),
            next_job: AtomicU64::new(1),
            ice_servers: Mutex::new(Vec::new()),
            webrtc: AtomicBool::new(cfg!(feature = "webrtc")),
        }
    }
}

impl Links {
    pub fn get(&self, node: &str) -> Option<Arc<NodeLink>> {
        self.nodes.lock().unwrap().get(node).cloned()
    }

    fn entry(&self, node: &str) -> Arc<NodeLink> {
        self.nodes
            .lock()
            .unwrap()
            .entry(node.to_string())
            .or_insert_with(|| Arc::new(NodeLink::new(node)))
            .clone()
    }

    pub fn connected(&self, node: &str) -> bool {
        self.get(node).map(|l| l.connected()).unwrap_or(false)
    }

    /// A one-time code to enrol `node` (valid 15 minutes).
    pub fn enrol_code(&self, node: &str) -> String {
        const A: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789";
        let mut b = [0u8; 10];
        let _ = getrandom::fill(&mut b);
        let code: String = b
            .iter()
            .map(|x| A[(*x as usize) % A.len()] as char)
            .collect();
        let code = format!("{}-{}", &code[..5], &code[5..]);
        let mut codes = self.codes.lock().unwrap();
        codes.retain(|_, (_, exp)| *exp > now());
        codes.insert(code.clone(), (node.to_string(), now() + 900));
        code
    }

    /// Use a code: true once, for the node it was made for.
    pub fn redeem(&self, node: &str, code: &str) -> bool {
        let mut codes = self.codes.lock().unwrap();
        match codes.get(code) {
            Some((n, exp)) if n == node && *exp > now() => {
                codes.remove(code);
                true
            }
            _ => false,
        }
    }

    /// Start a job on a connected node. Its envelope lines (the protocol of
    /// `indra node exec`) arrive on the receiver, ending with an exit line.
    pub fn start_job(
        &self,
        node: &str,
        job: &Job,
    ) -> Result<(u64, mpsc::UnboundedReceiver<String>)> {
        let link = self
            .get(node)
            .filter(|l| l.connected())
            .ok_or_else(|| format!("node '{}' is not connected", node))?;
        let id = self.next_job.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = mpsc::unbounded_channel();
        link.jobs
            .lock()
            .unwrap()
            .insert(id, JobSink { lines: tx, seq: 0 });
        link.send(json!({ "t": "job", "job": id, "spec": job }));
        Ok((id, rx))
    }

    pub fn job_input(&self, node: &str, job: u64, input: JobInput) {
        if let Some(l) = self.get(node) {
            l.send(match input {
                JobInput::Line(s) => json!({ "t": "in", "job": job, "l": s }),
                JobInput::Close => json!({ "t": "close", "job": job }),
                JobInput::Stop => json!({ "t": "stop", "job": job }),
            });
        }
    }

    /// A node connected: run its session until the connection ends.
    /// `enrolled` is a key to hand to a node that enrolled just now.
    pub async fn session(
        self: Arc<Self>,
        node: String,
        enrolled: Option<String>,
        socket: axum::extract::ws::WebSocket,
    ) {
        use axum::extract::ws::Message;
        use futures_util::{SinkExt, StreamExt};
        let link = self.entry(&node);
        let (mut sink, mut stream) = socket.split();
        let (tx, mut rx) = mpsc::unbounded_channel::<String>();
        let (inbound_tx, mut inbound) = mpsc::unbounded_channel::<String>();
        let epoch = link.epoch.fetch_add(1, Ordering::SeqCst) + 1;
        if let Some(key) = enrolled {
            let _ = tx.send(json!({ "t": "enrolled", "key": key }).to_string());
        }
        let ice = self.ice_servers.lock().unwrap().clone();
        let webrtc = self.webrtc.load(Ordering::SeqCst);
        let _ = tx.send(
            json!({ "t": "welcome", "hub": self.hub_id, "ice": ice_frame(&ice), "webrtc": webrtc })
                .to_string(),
        );
        *link.conn.lock().unwrap() = Some((epoch, tx.clone()));
        link.since.store(now(), Ordering::SeqCst);
        link.last_seen.store(now(), Ordering::SeqCst);
        for s in link.outbox.lock().unwrap().drain(..) {
            let _ = tx.send(s);
        }
        let mut tick = tokio::time::interval(HEARTBEAT);
        loop {
            tokio::select! {
                m = stream.next() => match m {
                    Some(Ok(Message::Text(t))) => {
                        link.last_seen.store(now(), Ordering::SeqCst);
                        self.frame(&link, t.as_str(), &tx, &inbound_tx).await;
                    }
                    Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
                    Some(Ok(_)) => link.last_seen.store(now(), Ordering::SeqCst),
                },
                Some(t) = inbound.recv() => {
                    link.last_seen.store(now(), Ordering::SeqCst);
                    self.frame(&link, &t, &tx, &inbound_tx).await;
                }
                Some(out) = rx.recv() => {
                    if sink.send(Message::Text(out.into())).await.is_err() {
                        break;
                    }
                }
                _ = tick.tick() => {
                    if now().saturating_sub(link.last_seen.load(Ordering::SeqCst)) > DEAD_AFTER {
                        break;
                    }
                    if sink.send(Message::Text(json!({ "t": "ping" }).to_string().into())).await.is_err() {
                        break;
                    }
                }
            }
        }
        // This connection is over (a newer one may already have replaced it).
        let current = {
            let mut c = link.conn.lock().unwrap();
            if c.as_ref().map(|(e, _)| *e == epoch).unwrap_or(false) {
                *c = None;
                true
            } else {
                false
            }
        };
        if !current {
            return;
        }
        #[cfg(feature = "webrtc")]
        {
            let p = link.peer.lock().unwrap().take();
            if let Some(p) = p {
                p.close().await;
            }
        }
        // Keep its jobs for a while; fail them if it does not come back.
        let l2 = link.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs(HUB_GRACE)).await;
            if l2.epoch.load(Ordering::SeqCst) == epoch && !l2.connected() {
                for (_, j) in l2.jobs.lock().unwrap().drain() {
                    let _ = j.lines.send(
                        json!({ "t": "error", "msg": format!("node '{}' went away", l2.name) })
                            .to_string(),
                    );
                    let _ = j.lines.send(json!({ "t": "exit", "code": -1 }).to_string());
                }
            }
        });
    }

    async fn frame(
        &self,
        link: &Arc<NodeLink>,
        text: &str,
        ws: &mpsc::UnboundedSender<String>,
        _inbound: &mpsc::UnboundedSender<String>,
    ) {
        let Ok(v) = serde_json::from_str::<Value>(text) else {
            return;
        };
        match v.get("t").and_then(Value::as_str).unwrap_or("") {
            "hello" => {
                *link.hello.lock().unwrap() = Some(v.clone());
                // Jobs the hub is waiting on that the node does not have were
                // lost on the way (the line dropped as they were sent): fail them.
                let known: Vec<u64> = v
                    .get("jobs")
                    .and_then(Value::as_array)
                    .map(|a| a.iter().filter_map(Value::as_u64).collect())
                    .unwrap_or_default();
                link.jobs.lock().unwrap().retain(|id, sink| {
                    let keep = known.contains(id);
                    if !keep {
                        let _ = sink.lines.send(json!({ "t": "error", "msg": "the job was lost while the node reconnected" }).to_string());
                        let _ = sink.lines.send(json!({ "t": "exit", "code": -1 }).to_string());
                    }
                    keep
                });
                #[cfg(feature = "webrtc")]
                if self.webrtc.load(Ordering::SeqCst)
                    && v.get("webrtc").and_then(Value::as_bool).unwrap_or(false)
                {
                    let ice = self.ice_servers.lock().unwrap().clone();
                    match rtc::Peer::new(&ice, ws.clone(), _inbound.clone(), true).await {
                        Ok(p) => *link.peer.lock().unwrap() = Some(p),
                        Err(e) => eprintln!("indra: node {}: {}", link.name, e),
                    }
                }
            }
            "res" => {
                let id = v.get("id").and_then(Value::as_u64).unwrap_or(0);
                if let Some(tx) = link.reqs.lock().unwrap().remove(&id) {
                    let r = if v.get("ok").and_then(Value::as_bool).unwrap_or(false) {
                        Ok(v.get("data").cloned().unwrap_or(Value::Null))
                    } else {
                        Err(v
                            .get("error")
                            .and_then(Value::as_str)
                            .unwrap_or("failed")
                            .to_string())
                    };
                    let _ = tx.send(r);
                }
            }
            "ev" => {
                let job = v.get("job").and_then(Value::as_u64).unwrap_or(0);
                let seq = v.get("seq").and_then(Value::as_u64).unwrap_or(0);
                let e = v.get("e").and_then(Value::as_str).unwrap_or("").to_string();
                let mut jobs = link.jobs.lock().unwrap();
                if let Some(sink) = jobs.get_mut(&job) {
                    if seq > sink.seq {
                        sink.seq = seq;
                        let exit = e.contains("\"t\":\"exit\"");
                        let _ = sink.lines.send(e);
                        if exit {
                            jobs.remove(&job);
                        }
                    }
                }
                drop(jobs);
                let _ = ws.send(json!({ "t": "ack", "job": job, "seq": seq }).to_string());
            }
            #[cfg(feature = "webrtc")]
            "rtc" => {
                let p = link.peer.lock().unwrap().clone();
                if let Some(p) = p {
                    if let Err(e) = p.on_signal(&v).await {
                        eprintln!("indra: node {}: {}", link.name, e);
                    }
                }
            }
            _ => {}
        }
    }
}

// ---- node side ---------------------------------------------------------------------

/// What a node keeps about its link to a hub, in an owner-only file.
#[derive(serde::Serialize, serde::Deserialize, Default)]
struct Saved {
    hub: String,
    name: String,
    key: String,
}

fn saved_path() -> std::path::PathBuf {
    crate::paths::config_dirs()
        .into_iter()
        .next()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join("agent.json")
}

struct NodeJob {
    input: mpsc::UnboundedSender<JobInput>,
    buf: VecDeque<(u64, String)>,
    seq: u64,
    done: bool,
}

/// A STUN or TURN server for WebRTC. TURN servers usually need a username
/// and credential; STUN servers do not.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct IceServer {
    pub urls: Vec<String>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub username: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub credential: String,
}

/// The `ice` list of a welcome frame: plain URLs (from older hubs, and for
/// servers without credentials) or objects.
pub fn parse_ice(v: Option<&Value>) -> Vec<IceServer> {
    v.and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|x| match x {
                    Value::String(u) => Some(IceServer {
                        urls: vec![u.clone()],
                        ..Default::default()
                    }),
                    Value::Object(_) => {
                        let mut x = x.clone();
                        // `urls` may be one string.
                        if let Some(u) = x.get("urls").and_then(Value::as_str).map(String::from) {
                            x["urls"] = json!([u]);
                        }
                        serde_json::from_value::<IceServer>(x).ok()
                    }
                    _ => None,
                })
                .filter(|s| !s.urls.is_empty())
                .collect()
        })
        .unwrap_or_default()
}

/// What a welcome frame carries: a bare URL where there is nothing else to
/// say (so older nodes still read it), an object where there is.
pub fn ice_frame(ice: &[IceServer]) -> Value {
    Value::Array(
        ice.iter()
            .flat_map(|s| {
                if s.username.is_empty() && s.credential.is_empty() {
                    s.urls.iter().map(|u| json!(u)).collect::<Vec<_>>()
                } else {
                    vec![json!(s)]
                }
            })
            .collect(),
    )
}

#[derive(Default)]
struct NodeState {
    jobs: Mutex<HashMap<u64, NodeJob>>,
    hub: Mutex<Option<String>>,
    /// STUN/TURN servers the hub announced.
    ice: Mutex<Vec<IceServer>>,
}

#[cfg(feature = "webrtc")]
type NodePeer = Option<Arc<rtc::Peer>>;
#[cfg(not(feature = "webrtc"))]
type NodePeer = Option<()>;

impl NodeState {
    fn stop_all(&self) {
        for (_, j) in self.jobs.lock().unwrap().drain() {
            let _ = j.input.send(JobInput::Stop);
        }
    }
}

pub struct ConnectOpts {
    pub hub: String,
    pub name: String,
    pub code: Option<String>,
    pub targets: Vec<String>,
    /// Use the WebRTC data channel when the hub offers one.
    pub webrtc: bool,
}

/// `indra node connect`: keep a link to the hub open and run its jobs.
pub async fn connect(settings: crate::deck::config::Settings, o: ConnectOpts) -> Result<()> {
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;
    use tokio_tungstenite::tungstenite::http::HeaderValue;
    use tokio_tungstenite::tungstenite::Message;

    crate::deck::nodes::validate_name(&o.name)?;
    let base = o.hub.trim_end_matches('/');
    let url = if let Some(r) = base.strip_prefix("https://") {
        format!("wss://{}/agent/connect", r)
    } else if let Some(r) = base.strip_prefix("http://") {
        format!("ws://{}/agent/connect", r)
    } else {
        return Err("the hub URL must start with http:// or https://".into());
    };
    let path = saved_path();
    let mut key: Option<String> = std::fs::read_to_string(&path)
        .ok()
        .and_then(|t| serde_json::from_str::<Saved>(&t).ok())
        .filter(|s| s.hub == base && s.name == o.name)
        .map(|s| s.key);
    let mut code = o.code.clone();
    if key.is_none() && code.is_none() {
        return Err(format!(
            "not enrolled with {} as '{}': add the node in the console (Nodes) and pass --code",
            base, o.name
        ));
    }
    let settings = Arc::new(settings);
    let state = Arc::new(NodeState::default());
    let (out_tx, mut out_rx) = mpsc::unbounded_channel::<(u64, u64)>();
    let mut backoff = 1u64;
    let mut away_since: Option<u64> = None;
    loop {
        if let Some(t) = away_since {
            if now().saturating_sub(t) > NODE_GRACE {
                state.stop_all();
            }
        }
        let mut req = url
            .as_str()
            .into_client_request()
            .map_err(|e| e.to_string())?;
        let h = req.headers_mut();
        h.insert(
            "x-indra-node",
            HeaderValue::from_str(&o.name).map_err(|e| e.to_string())?,
        );
        if let Some(k) = &key {
            h.insert(
                "authorization",
                HeaderValue::from_str(&format!("Bearer {}", k)).map_err(|e| e.to_string())?,
            );
        } else if let Some(c) = &code {
            h.insert(
                "x-indra-enroll",
                HeaderValue::from_str(c).map_err(|e| e.to_string())?,
            );
        }
        let ws = match tokio_tungstenite::connect_async(req).await {
            Ok((ws, _)) => ws,
            Err(e) => {
                let msg = e.to_string();
                if msg.contains("401") && code.is_some() && key.is_none() {
                    return Err(
                        "the hub refused the enrolment code (wrong, used, or expired)".into(),
                    );
                }
                eprintln!(
                    "indra: cannot reach {}: {}; retrying in {}s",
                    base, msg, backoff
                );
                away_since.get_or_insert(now());
                tokio::time::sleep(jitter(backoff)).await;
                backoff = (backoff * 2).min(60);
                continue;
            }
        };
        let connected_at = now();
        away_since = None;
        eprintln!("indra: connected to {} as {}", base, o.name);
        let (mut sink, mut stream) = ws.split();
        let (tx, mut rx) = mpsc::unbounded_channel::<String>();
        let (inbound_tx, mut inbound) = mpsc::unbounded_channel::<String>();
        let mut peer: NodePeer = None;
        let hello = json!({
            "t": "hello", "version": env!("CARGO_PKG_VERSION"), "info": crate::deck::nodes::local_info(&settings),
            "targets": o.targets, "webrtc": o.webrtc && cfg!(feature = "webrtc"),
            "jobs": state.jobs.lock().unwrap().keys().collect::<Vec<_>>(),
        });
        if sink
            .send(Message::Text(hello.to_string().into()))
            .await
            .is_err()
        {
            tokio::time::sleep(jitter(backoff)).await;
            continue;
        }
        // Whatever the hub has not acknowledged, again.
        let mut sent: HashMap<u64, u64> = HashMap::new();
        for (id, j) in state.jobs.lock().unwrap().iter() {
            for (seq, e) in &j.buf {
                let _ = tx.send(json!({ "t": "ev", "job": id, "seq": seq, "e": e }).to_string());
                sent.insert(*id, *seq);
            }
        }
        let mut last_seen = now();
        let mut tick = tokio::time::interval(HEARTBEAT);
        loop {
            tokio::select! {
                m = stream.next() => match m {
                    Some(Ok(Message::Text(t))) => {
                        last_seen = now();
                        let t = t.to_string();
                        node_frame(&t, &state, &settings, &out_tx, &tx, &mut key, &mut code, base, &o.name, &mut peer, &inbound_tx).await;
                    }
                    Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
                    Some(Ok(_)) => last_seen = now(),
                },
                Some(t) = inbound.recv() => {
                    last_seen = now();
                    node_frame(&t, &state, &settings, &out_tx, &tx, &mut key, &mut code, base, &o.name, &mut peer, &inbound_tx).await;
                }
                Some((job, seq)) = out_rx.recv() => {
                    if sent.get(&job).copied().unwrap_or(0) >= seq {
                        continue;
                    }
                    let e = state.jobs.lock().unwrap().get(&job).and_then(|j| j.buf.iter().find(|(s, _)| *s == seq).map(|(_, e)| e.clone()));
                    if let Some(e) = e {
                        let f = json!({ "t": "ev", "job": job, "seq": seq, "e": e }).to_string();
                        #[cfg(feature = "webrtc")]
                        let direct = peer.as_ref().map(|p| p.send(&f)).unwrap_or(false);
                        #[cfg(not(feature = "webrtc"))]
                        let direct = false;
                        if !direct {
                            let _ = tx.send(f);
                        }
                        sent.insert(job, seq);
                    }
                }
                Some(s) = rx.recv() => {
                    if sink.send(Message::Text(s.into())).await.is_err() {
                        break;
                    }
                }
                _ = tick.tick() => {
                    if now().saturating_sub(last_seen) > DEAD_AFTER {
                        eprintln!("indra: no word from the hub for {}s; reconnecting", DEAD_AFTER);
                        break;
                    }
                }
            }
        }
        #[cfg(feature = "webrtc")]
        if let Some(p) = peer.take() {
            p.close().await;
        }
        away_since = Some(now());
        if now().saturating_sub(connected_at) > 30 {
            backoff = 1;
        }
        eprintln!("indra: lost the hub; reconnecting in about {}s", backoff);
        tokio::time::sleep(jitter(backoff)).await;
        backoff = (backoff * 2).min(60);
    }
}

/// `secs` with up to 30% random jitter, so many nodes do not reconnect at once.
fn jitter(secs: u64) -> Duration {
    let mut b = [0u8; 2];
    let _ = getrandom::fill(&mut b);
    let r = u16::from_le_bytes(b) as f64 / u16::MAX as f64;
    Duration::from_millis((secs as f64 * 1000.0 * (0.85 + 0.3 * r)) as u64)
}

#[allow(clippy::too_many_arguments)]
async fn node_frame(
    text: &str,
    state: &Arc<NodeState>,
    settings: &Arc<crate::deck::config::Settings>,
    out_tx: &mpsc::UnboundedSender<(u64, u64)>,
    ws: &mpsc::UnboundedSender<String>,
    key: &mut Option<String>,
    code: &mut Option<String>,
    hub: &str,
    name: &str,
    peer: &mut NodePeer,
    _inbound: &mpsc::UnboundedSender<String>,
) -> Option<()> {
    let _ = &peer;
    let v: Value = serde_json::from_str(text).ok()?;
    let job = v.get("job").and_then(Value::as_u64).unwrap_or(0);
    match v.get("t").and_then(Value::as_str).unwrap_or("") {
        "enrolled" => {
            let k = v.get("key").and_then(Value::as_str)?.to_string();
            let saved = Saved {
                hub: hub.into(),
                name: name.into(),
                key: k.clone(),
            };
            let p = saved_path();
            if let Some(d) = p.parent() {
                let _ = std::fs::create_dir_all(d);
            }
            let text = serde_json::to_string(&saved).unwrap_or_default();
            let mut opts = std::fs::OpenOptions::new();
            opts.write(true).create(true).truncate(true);
            #[cfg(unix)]
            std::os::unix::fs::OpenOptionsExt::mode(&mut opts, 0o600);
            match opts.open(&p) {
                Ok(mut f) => {
                    use std::io::Write;
                    let _ = f.write_all(text.as_bytes());
                }
                Err(e) => eprintln!("indra: cannot save the node key to {}: {}", p.display(), e),
            }
            *key = Some(k);
            *code = None;
            eprintln!("indra: enrolled; the key is in {}", p.display());
        }
        "welcome" => {
            *state.ice.lock().unwrap() = parse_ice(v.get("ice"));
            let id = v
                .get("hub")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let mut hub_id = state.hub.lock().unwrap();
            if hub_id.as_deref().map(|h| h != id).unwrap_or(false) {
                // A different hub process: the jobs it started are nobody's now.
                state.stop_all();
            }
            *hub_id = Some(id);
        }
        "req" => {
            // What `indra node runs|info` answer over SSH, for this link.
            let id = v.get("id").and_then(Value::as_u64).unwrap_or(0);
            let args = v.get("args").cloned().unwrap_or(Value::Null);
            let r: Result<Value> = match v.get("what").and_then(Value::as_str).unwrap_or("") {
                "runs" => serde_json::from_value::<crate::deck::nodes::RunsSpec>(args)
                    .map_err(|e| format!("bad runs request: {}", e))
                    .map(crate::deck::nodes::runs_for),
                "info" => serde_json::to_value(crate::deck::nodes::local_info(settings))
                    .map_err(|e| e.to_string()),
                "run_files" => serde_json::from_value::<crate::deck::nodes::RunFilesReq>(args)
                    .map_err(|e| format!("bad run_files request: {}", e))
                    .and_then(crate::deck::nodes::run_files),
                other => Err(format!("unknown request {:?}", other)),
            };
            let f = match r {
                Ok(d) => json!({ "t": "res", "id": id, "ok": true, "data": d }),
                Err(e) => json!({ "t": "res", "id": id, "ok": false, "error": e }),
            };
            let _ = ws.send(f.to_string());
        }
        "ping" => {
            let _ = ws.send(json!({ "t": "pong" }).to_string());
        }
        "ack" => {
            let seq = v.get("seq").and_then(Value::as_u64).unwrap_or(0);
            let mut jobs = state.jobs.lock().unwrap();
            let remove = if let Some(j) = jobs.get_mut(&job) {
                while j.buf.front().map(|(s, _)| *s <= seq).unwrap_or(false) {
                    j.buf.pop_front();
                }
                j.done && j.buf.is_empty()
            } else {
                false
            };
            if remove {
                jobs.remove(&job);
            }
        }
        "job" => {
            let spec: Job = serde_json::from_value(v.get("spec").cloned()?).ok()?;
            let (itx, irx) = mpsc::unbounded_channel();
            state.jobs.lock().unwrap().insert(
                job,
                NodeJob {
                    input: itx,
                    buf: VecDeque::new(),
                    seq: 0,
                    done: false,
                },
            );
            let (st, out, s) = (state.clone(), out_tx.clone(), settings.clone());
            let emit: crate::deck::nodes::Emit = {
                let (st, out) = (st.clone(), out.clone());
                Arc::new(move |e: Value| {
                    let mut jobs = st.jobs.lock().unwrap();
                    if let Some(j) = jobs.get_mut(&job) {
                        j.seq += 1;
                        j.buf.push_back((j.seq, e.to_string()));
                        while j.buf.len() > BUFFER {
                            j.buf.pop_front();
                        }
                        let _ = out.send((job, j.seq));
                    }
                })
            };
            tokio::spawn(async move {
                crate::deck::nodes::run_job(&s, spec, emit, irx).await;
                if let Some(j) = st.jobs.lock().unwrap().get_mut(&job) {
                    j.done = true;
                }
            });
        }
        "in" | "close" | "stop" => {
            let input = match v["t"].as_str() {
                Some("in") => {
                    JobInput::Line(v.get("l").and_then(Value::as_str).unwrap_or("").to_string())
                }
                Some("close") => JobInput::Close,
                _ => JobInput::Stop,
            };
            if let Some(j) = state.jobs.lock().unwrap().get(&job) {
                let _ = j.input.send(input);
            }
        }
        #[cfg(feature = "webrtc")]
        "rtc" => {
            if peer.is_none() && v.get("sdp").is_some() {
                let ice = state.ice.lock().unwrap().clone();
                match rtc::Peer::new(&ice, ws.clone(), _inbound.clone(), false).await {
                    Ok(p) => *peer = Some(p),
                    Err(e) => eprintln!("indra: {}", e),
                }
            }
            if let Some(p) = peer.as_ref() {
                if let Err(e) = p.on_signal(&v).await {
                    eprintln!("indra: {}", e);
                }
            }
        }
        _ => {}
    }
    None
}
