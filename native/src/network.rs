use crate::transport::{Endpoint, PendingConnection, moq, runtime::Runtime, web};
use crate::{
    api::{Change, Command, Readers, emit, fallback},
    discovery::Advertisement,
    timeline::{Anchor, FollowerCore, Scheduled, Timeline, View},
    tracking::Tracker,
    worker::Job,
    *,
};
use std::{
    future::Future,
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc as sync,
    },
    task::{Context, Poll},
    time::{Duration, Instant},
};
use tidkod_protocol::{decode_probe, decode_snapshot, encode};
use web_transport_trait::poll::Session as _;
fn transport(e: impl std::fmt::Display) -> Error {
    Error::Transport(e.to_string())
}
fn version() -> moq::Version {
    "moq-lite-05".parse().unwrap()
}
fn stamp(clock: MonotonicClock) -> moq::Timestamp {
    moq::Timestamp::from_nanos(clock.now_ns()).unwrap()
}
type Pending<T> = Pin<Box<dyn Future<Output = Result<T>>>>;
fn origin() -> (moq::origin::Producer, moq::origin::Driver) {
    let id = (uuid::Uuid::new_v4().as_u128() as u64) & ((1_u64 << 62) - 1);
    let mut config = moq::origin::Config::new(moq::Hop::new(id.max(1)).unwrap());
    config.cache_duration = Duration::from_secs(2);
    config.pool =
        moq::cache::Pool::new(moq::cache::Config::default().with_capacity(256 * 1024_u64));
    moq::origin::Producer::new(config)
}
async fn subscribe(origin: moq::origin::Producer, name: &str) -> Result<moq::track::Subscriber> {
    let b = origin
        .consume()
        .routed_broadcast("tidkod/v1")
        .await
        .map_err(transport)?;
    b.track(name)
        .map_err(transport)?
        .subscribe(None)
        .await
        .map_err(transport)
}
struct Io {
    endpoint: Endpoint,
    runtime: Runtime,
    web: web::Driver,
    park: moq::kio::Park,
}
impl Io {
    fn new(mut endpoint: Endpoint, registry: &mio::Registry, token: mio::Token) -> Result<Self> {
        endpoint.register(registry, token)?;
        let runtime = Runtime::default();
        let web = web::Driver::new(runtime.timers());
        Ok(Self {
            endpoint,
            runtime,
            web,
            park: Default::default(),
        })
    }
    fn step(&mut self, cx: &mut Context<'_>) -> Result<()> {
        self.endpoint.step(Instant::now())?;
        self.runtime.timers().advance(Instant::now());
        self.runtime.step(cx, &mut self.park);
        self.web.step(cx, Instant::now());
        Ok(())
    }
    /// Send newly queued probe datagrams in this pass. Probe timestamps are taken in the
    /// application; every pass between them and the socket adds unobservable path asymmetry.
    fn flush(&mut self, cx: &mut Context<'_>) -> Result<()> {
        // Restore transport budgets that earlier work in this pass may have exhausted.
        self.endpoint.drive(Instant::now())?;
        self.runtime.step(cx, &mut self.park);
        self.endpoint.drive(Instant::now())?;
        Ok(())
    }
    fn deadline(&self) -> Option<Instant> {
        [
            self.endpoint.next_deadline(),
            self.runtime.deadline(),
            self.runtime.timers().advance(Instant::now()),
            self.web.next_deadline(),
        ]
        .into_iter()
        .flatten()
        .min()
    }
}
impl Drop for Io {
    fn drop(&mut self) {
        self.runtime.clear();
        self.web.clear();
    }
}
struct Peer {
    connection: Option<crate::transport::Connection>,
    tls: Option<PendingConnection>,
    handshake: Option<Pending<moq::Session>>,
    session: Option<moq::Session>,
    publish: moq::origin::Producer,
    ingest: moq::origin::Producer,
    drivers: [moq::origin::Driver; 2],
    _broadcast: moq::broadcast::Producer,
    state: Option<moq::track::Producer>,
    datagrams: moq::track::Producer,
    subscriptions: Option<Pending<Vec<moq::track::Subscriber>>>,
    incoming: Vec<moq::track::Subscriber>,
    leader: bool,
    deadline: Instant,
    park: moq::kio::Park,
    driver_park: moq::kio::Park,
    driver_deadline: Option<Instant>,
}
impl Peer {
    fn new(
        tls: PendingConnection,
        leader: bool,
        timeout: Duration,
        clock: MonotonicClock,
        state: Option<&[u8]>,
    ) -> Result<Self> {
        let (publish, a) = origin();
        let (ingest, b) = origin();
        let broadcast = publish.create_broadcast("tidkod/v1").map_err(transport)?;
        let mut snapshots = if leader {
            Some(broadcast.create_track("state", None).map_err(transport)?)
        } else {
            None
        };
        if let (Some(track), Some(bytes)) = (&mut snapshots, state) {
            track.write_frame(stamp(clock), bytes).map_err(transport)?;
        }
        let datagrams = broadcast
            .create_track(
                if leader {
                    "clock/reply"
                } else {
                    "clock/request"
                },
                None,
            )
            .map_err(transport)?;
        broadcast.announce(Default::default()).map_err(transport)?;
        Ok(Self {
            connection: None,
            tls: Some(tls),
            handshake: None,
            session: None,
            publish,
            ingest,
            drivers: [a, b],
            _broadcast: broadcast,
            state: snapshots,
            datagrams,
            subscriptions: None,
            incoming: Vec::new(),
            leader,
            deadline: Instant::now() + timeout,
            park: Default::default(),
            driver_park: Default::default(),
            driver_deadline: None,
        })
    }
    fn step(&mut self, io: &Io, cx: &mut Context<'_>) -> Result<bool> {
        let waiter = self.driver_park.hold(cx);
        self.driver_deadline = None;
        let now = Instant::now();
        for driver in &mut self.drivers {
            let at = driver.poll(now, waiter).map_err(transport)?;
            self.driver_deadline = self.driver_deadline.into_iter().chain(at).min();
        }
        if self.incoming.is_empty() && Instant::now() >= self.deadline {
            return Err(transport("connection/subscription timeout"));
        }
        if let Some(connection) = self
            .tls
            .as_ref()
            .map(|p| p.ready())
            .transpose()
            .map_err(transport)?
            .flatten()
        {
            self.tls = None;
            self.connection = Some(connection.clone());
            let web = io.web.clone();
            let runtime = io.runtime.clone();
            let publish = self.publish.clone();
            let ingest = self.ingest.clone();
            let leader = self.leader;
            self.handshake = Some(Box::pin(async move {
                let session = if connection.protocol() == Some("h3") {
                    if !leader {
                        return Err(transport("unexpected HTTP/3 client ALPN"));
                    }
                    let request = web::Request::accept(&web, connection)
                        .await
                        .map_err(transport)?;
                    request
                        .respond(web::Response::default().with_protocol("moq-lite-05"))
                        .await
                        .map_err(transport)?
                } else {
                    web::Session::raw(connection)
                };
                let (session, driver) = if leader {
                    moq::Server::new()
                        .with_versions(vec![version()].into())
                        .with_publisher(&publish)
                        .with_subscriber(ingest)
                        .accept_lite(Instant::now(), session)
                        .await
                        .map_err(transport)
                } else {
                    moq::Client::new()
                        .with_versions(vec![version()].into())
                        .with_publisher(&publish)
                        .with_subscriber(ingest)
                        .connect_lite(Instant::now(), session)
                        .await
                        .map_err(transport)
                }?;
                runtime.spawn(driver);
                Ok(session)
            }));
        }
        if let Some(future) = &mut self.handshake
            && let Poll::Ready(result) = future.as_mut().poll(cx)
        {
            self.session = Some(result?);
            self.handshake = None;
            let ingest = self.ingest.clone();
            let leader = self.leader;
            self.subscriptions = Some(Box::pin(async move {
                if leader {
                    Ok(vec![subscribe(ingest, "clock/request").await?])
                } else {
                    let states = subscribe(ingest.clone(), "state").await?;
                    let replies = subscribe(ingest, "clock/reply").await?;
                    Ok(vec![states, replies])
                }
            }));
        }
        if let Some(session) = &self.session {
            let mut closed = std::pin::pin!(session.closed());
            if let Poll::Ready(e) = closed.as_mut().poll(cx) {
                return Err(transport(e));
            }
        }
        if let Some(future) = &mut self.subscriptions
            && let Poll::Ready(result) = future.as_mut().poll(cx)
        {
            self.incoming = result?;
            self.subscriptions = None;
        }
        Ok(!self.incoming.is_empty())
    }
}
fn leader_view(t: Timeline, now: u64) -> View {
    View {
        local_clock: true,
        timeline: t,
        mapping: clock::ClockMapping {
            reference_ns: now,
            offset_ns: 0.,
            drift: 0.,
            uncertainty_ns: 0.,
            last_sample_ns: now,
            converged: true,
            accepted_observations: 0,
            evidence: None,
        },
        connection: ConnectionState::Connected,
        sync: SyncState::Synchronized,
        ..Default::default()
    }
}
pub(crate) struct LeaderJob {
    config: LeaderConfig,
    commands: sync::Receiver<Command>,
    events: sync::SyncSender<Event>,
    stop: Arc<AtomicBool>,
    clock: MonotonicClock,
    io: Io,
    peers: Vec<Peer>,
    timeline: Timeline,
    readers: Readers,
    tracker: Tracker,
    bytes: Vec<u8>,
    last_publish: Instant,
    dirty: bool,
    source_deadline: Option<Instant>,
    command_more: bool,
    _advertisement: Option<Advertisement>,
}
impl LeaderJob {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        config: LeaderConfig,
        commands: sync::Receiver<Command>,
        events: sync::SyncSender<Event>,
        stop: Arc<AtomicBool>,
        clock: MonotonicClock,
        ready: sync::SyncSender<Result<LeaderInfo>>,
        registry: &mio::Registry,
        token: mio::Token,
    ) -> Option<Self> {
        let create = || -> Result<(Io, LeaderInfo)> {
            let (server, fingerprint) = crate::transport::tls::server().map_err(transport)?;
            let endpoint = Endpoint::new(config.bind, Some(server), config.max_followers)?;
            let info = LeaderInfo {
                address: endpoint.local_addr()?,
                identity: config.identity.clone(),
                session: *uuid::Uuid::new_v4().as_bytes(),
                fingerprint,
            };
            Ok((Io::new(endpoint, registry, token)?, info))
        };
        let (io, info) = match create() {
            Ok(v) => v,
            Err(e) => {
                let _ = ready.send(Err(e));
                return None;
            }
        };
        let advertisement = if config.advertise {
            match Advertisement::new(&config, &info) {
                Ok(a) => Some(a),
                Err(e) => {
                    emit(&events, Event::Error(e.to_string()));
                    None
                }
            }
        } else {
            None
        };
        let timeline = Timeline {
            session: info.session,
            session_id: Some(
                config
                    .session_id
                    .unwrap_or_else(|| *uuid::Uuid::new_v4().as_bytes()),
            ),
            revision: 1,
            format: config.format,
            anchor: Anchor {
                time_ns: clock.now_ns(),
                position: config.position,
                rate: config.rate,
            },
            source_kind: config.source_kind,
            source_health: if config.source_kind == SourceKind::Tracked {
                SourceHealth::Degraded
            } else {
                SourceHealth::Healthy
            },
            ..Default::default()
        };
        let bytes = encode(&timeline.wire()).unwrap();
        let _ = ready.send(Ok(info));
        Some(Self {
            config,
            commands,
            events,
            stop,
            clock,
            io,
            peers: Vec::new(),
            timeline,
            readers: Readers::new(clock),
            tracker: Tracker::default(),
            bytes,
            last_publish: Instant::now(),
            dirty: false,
            source_deadline: None,
            command_more: false,
            _advertisement: advertisement,
        })
    }
    fn progress(&mut self, cx: &mut Context<'_>) -> Result<()> {
        self.io.step(cx)?;
        let mut publish = false;
        self.command_more = false;
        for operation in 0..32 {
            let Ok(command) = self.commands.try_recv() else {
                break;
            };
            self.command_more = operation == 31;
            match command {
                Command::Reader(tx) => self
                    .readers
                    .add(leader_view(self.timeline, self.clock.now_ns()), tx),
                Command::SessionId(id, reply) => {
                    publish |= self.timeline.session_id != Some(id);
                    self.timeline.session_id = Some(id);
                    self.readers
                        .publish(leader_view(self.timeline, self.clock.now_ns()));
                    let _ = reply.send(Ok(()));
                }
                Command::Change(change, effective, reply) => {
                    let result =
                        change_timeline(&mut self.timeline, change, effective, self.clock.now_ns());
                    publish |= result.is_ok();
                    self.readers
                        .publish(leader_view(self.timeline, self.clock.now_ns()));
                    let _ = reply.send(result);
                }
                Command::Sample(sample, reply) => {
                    let old = self.timeline.source_health;
                    let result =
                        self.tracker
                            .sample(&mut self.timeline, sample, self.clock.now_ns());
                    if let Ok(immediate) = result {
                        self.dirty = true;
                        publish |= immediate || old != self.timeline.source_health;
                        self.source_deadline = Some(
                            Instant::now()
                                + Duration::from_nanos(
                                    sample
                                        .timestamp_ns
                                        .saturating_add(
                                            self.config.timing.source_timeout.as_nanos() as u64,
                                        )
                                        .saturating_add(1)
                                        .saturating_sub(self.clock.now_ns()),
                                ),
                        );
                    }
                    self.readers
                        .publish(leader_view(self.timeline, self.clock.now_ns()));
                    let _ = reply.send(result.map(|_| ()).map_err(Into::into));
                }
                Command::Reconnect => {}
            }
        }
        let now = self.clock.now_ns();
        while self.timeline.scheduled_len > 0 && self.timeline.scheduled[0].anchor.time_ns <= now {
            let s = self.timeline.scheduled[0];
            self.timeline.anchor = s.anchor;
            self.timeline.discontinuity = s.discontinuity;
            self.timeline.scheduled.rotate_left(1);
            self.timeline.scheduled_len -= 1;
            publish = true;
        }
        if self.config.source_kind == SourceKind::Tracked {
            let health = self
                .tracker
                .health(now, self.config.timing.source_timeout.as_nanos() as u64);
            if health != self.timeline.source_health {
                self.timeline.source_health = health;
                emit(&self.events, Event::SourceHealth(health));
                publish = true;
            }
            if self.source_deadline.is_some_and(|t| t <= Instant::now()) {
                self.source_deadline = None;
            }
        }
        publish |= self.last_publish.elapsed() >= self.config.timing.heartbeat
            || (self.dirty && self.last_publish.elapsed() >= self.config.timing.tracked_publish);
        if publish {
            self.timeline.revision += 1;
            self.bytes = encode(&self.timeline.wire())?;
            self.last_publish = Instant::now();
            self.dirty = false;
        }
        while let Some(connection) = self.io.endpoint.accept() {
            match Peer::new(
                connection,
                true,
                self.config.timing.connect_timeout,
                self.clock,
                Some(&self.bytes),
            ) {
                Ok(peer) => self.peers.push(peer),
                Err(e) => emit(&self.events, Event::Error(e.to_string())),
            }
        }
        let mut replied = false;
        self.peers.retain_mut(|peer| {
            let result = (|| -> Result<()> {
                if publish {
                    peer.state
                        .as_mut()
                        .unwrap()
                        .write_frame(stamp(self.clock), self.bytes.as_slice())
                        .map_err(transport)?;
                }
                if !peer.step(&self.io, cx)? {
                    return Ok(());
                }
                let waiter = peer.park.hold(cx);
                for batch in 0..8 {
                    let d = match peer.incoming[0].poll_recv_datagram(waiter) {
                        Poll::Pending => break,
                        Poll::Ready(r) => r
                            .map_err(transport)?
                            .ok_or_else(|| transport("probe track ended"))?,
                    };
                    if batch == 7 {
                        cx.waker().wake_by_ref();
                    }
                    let t2 = self.clock.now_ns();
                    let mut p = decode_probe(&d.payload)?;
                    if p.t2 != 0 || p.t3 != 0 {
                        return Err(Error::Invalid("probe request includes reply timestamps"));
                    }
                    p.t2 = t2;
                    let timestamp = stamp(self.clock);
                    p.t3 = self.clock.now_ns();
                    peer.datagrams
                        .append_datagram(timestamp, encode(&p)?)
                        .map_err(transport)?;
                    replied = true;
                }
                Ok(())
            })();
            if let Err(e) = result {
                emit(&self.events, Event::Error(e.to_string()));
                false
            } else {
                true
            }
        });
        if replied {
            self.io.flush(cx)?;
        }
        self.readers
            .publish(leader_view(self.timeline, self.clock.now_ns()));
        Ok(())
    }
}
impl Job for LeaderJob {
    fn step(&mut self, cx: &mut Context<'_>) -> bool {
        if self.stop.load(Ordering::Acquire) {
            return false;
        }
        if let Err(e) = self.progress(cx) {
            emit(&self.events, Event::Error(e.to_string()));
            return false;
        }
        true
    }
    fn deadline(&self) -> Option<Instant> {
        let mut next = self.last_publish + self.config.timing.heartbeat;
        if self.dirty {
            next = next.min(self.last_publish + self.config.timing.tracked_publish);
        }
        if self.timeline.scheduled_len > 0 {
            next = next.min(
                Instant::now()
                    + Duration::from_nanos(
                        self.timeline.scheduled[0]
                            .anchor
                            .time_ns
                            .saturating_sub(self.clock.now_ns()),
                    ),
            );
        }
        for deadline in [
            self.io.deadline(),
            self.source_deadline,
            self.peers.iter().filter_map(|p| p.driver_deadline).min(),
            self.peers
                .iter()
                .filter(|p| p.incoming.is_empty())
                .map(|p| p.deadline)
                .min(),
        ]
        .into_iter()
        .flatten()
        {
            next = next.min(deadline);
        }
        Some(next)
    }
    fn ready(&self) -> bool {
        self.command_more || self.io.endpoint.needs_pass() || self.io.runtime.needs_pass()
    }
}
impl Drop for LeaderJob {
    fn drop(&mut self) {
        let mut view = leader_view(self.timeline, self.clock.now_ns());
        view.connection = ConnectionState::Shutdown;
        self.readers.publish(view);
    }
}

pub(crate) struct FollowerJob {
    config: FollowerConfig,
    commands: sync::Receiver<Command>,
    events: sync::SyncSender<Event>,
    stop: Arc<AtomicBool>,
    clock: MonotonicClock,
    io: Io,
    peer: Option<Peer>,
    core: FollowerCore,
    readers: Readers,
    probes: tidkod_protocol::probes::Probes,
    next_probe: Instant,
    retry: Instant,
    backoff: Duration,
    began: Instant,
    connected: bool,
    group: Option<moq::group::Consumer>,
    frame: Option<moq::frame::Consumer>,
    frame_deadline: Option<Instant>,
    command_more: bool,
}
impl FollowerJob {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        config: FollowerConfig,
        commands: sync::Receiver<Command>,
        events: sync::SyncSender<Event>,
        stop: Arc<AtomicBool>,
        clock: MonotonicClock,
        registry: &mio::Registry,
        token: mio::Token,
    ) -> Option<Self> {
        let io = Endpoint::new(config.bind, None, 4)
            .map_err(Error::from)
            .and_then(|e| Io::new(e, registry, token));
        let io = match io {
            Ok(io) => io,
            Err(e) => {
                emit(&events, Event::Error(e.to_string()));
                return None;
            }
        };
        let core = FollowerCore::new(fallback(&config), config.correction);
        let backoff = config.timing.retry_min;
        Some(Self {
            config,
            commands,
            events,
            stop,
            clock,
            io,
            peer: None,
            core,
            readers: Readers::new(clock),
            probes: Default::default(),
            next_probe: Instant::now(),
            retry: Instant::now(),
            backoff,
            began: Instant::now(),
            connected: false,
            group: None,
            frame: None,
            frame_deadline: None,
            command_more: false,
        })
    }
    fn disconnect(&mut self) {
        self.peer = None;
        self.group = None;
        self.frame = None;
        self.frame_deadline = None;
        self.connected = false;
        self.probes = Default::default();
        self.core.disconnected();
        emit(
            &self.events,
            Event::Connection(ConnectionState::Disconnected),
        );
        if self.began.elapsed() > Duration::from_secs(2) {
            self.backoff = self.config.timing.retry_min;
        }
        self.retry = Instant::now() + self.backoff;
        self.backoff = self
            .backoff
            .saturating_mul(2)
            .min(self.config.timing.retry_max);
    }
    fn progress(&mut self, cx: &mut Context<'_>) -> Result<()> {
        self.io.step(cx)?;
        self.command_more = false;
        for operation in 0..32 {
            let Ok(command) = self.commands.try_recv() else {
                break;
            };
            self.command_more = operation == 31;
            match command {
                Command::Reader(tx) => self.readers.add(self.core.view, tx),
                Command::Reconnect => {
                    self.disconnect();
                    self.retry = Instant::now();
                }
                Command::Change(_, _, tx) | Command::Sample(_, tx) | Command::SessionId(_, tx) => {
                    let _ = tx.send(Err(Error::Invalid("follower cannot lead")));
                }
            }
        }
        if self.peer.is_none() && Instant::now() >= self.retry {
            self.core.view.connection = ConnectionState::Connecting;
            emit(&self.events, Event::Connection(ConnectionState::Connecting));
            self.began = Instant::now();
            let pin = match &self.config.trust {
                Trust::TrustedLan => None,
                Trust::Pinned(p) => Some(p.as_str()),
            };
            let pending = self
                .io
                .endpoint
                .connect(self.config.address, pin)
                .map_err(transport)?;
            self.peer = Some(Peer::new(
                pending,
                false,
                self.config.timing.connect_timeout,
                self.clock,
                None,
            )?);
        }
        if let Some(peer) = &mut self.peer
            && peer.step(&self.io, cx)?
        {
            if !self.connected {
                self.connected = true;
                self.core.connected();
                emit(&self.events, Event::Connection(ConnectionState::Connected));
                self.next_probe = Instant::now();
            }
            let waiter = peer.park.hold(cx);
            // Probes stay serviceable even when a snapshot is only partly received.
            for batch in 0..8 {
                let d = match peer.incoming[1].poll_recv_datagram(waiter) {
                    Poll::Pending => break,
                    Poll::Ready(r) => r
                        .map_err(transport)?
                        .ok_or_else(|| transport("reply track ended"))?,
                };
                if batch == 7 {
                    cx.waker().wake_by_ref();
                }
                let t4 = self.clock.now_ns();
                if let Some(exchange) = self.probes.reply(&d.payload, t4)? {
                    if let Some(e) = self.core.measurement_timed(
                        exchange,
                        self.probes.last_publication_ns(),
                        self.clock.now_ns(),
                    ) {
                        emit(&self.events, Event::Correction(e));
                    }
                    if self.config.clock_diagnostics
                        && let Some(observation) = self.core.estimator.trace().last()
                        && observation.exchange.t4 == exchange.t4
                    {
                        emit(&self.events, Event::ClockObservation(*observation));
                    }
                }
            }
            // Keep only the newest available independent state group.
            for batch in 0..8 {
                match peer.incoming[0].poll_recv_group(waiter) {
                    Poll::Pending => break,
                    Poll::Ready(r) => {
                        if batch == 7 {
                            cx.waker().wake_by_ref();
                        }
                        self.group = Some(
                            r.map_err(transport)?
                                .ok_or_else(|| transport("state track ended"))?,
                        );
                        self.frame = None;
                        self.frame_deadline =
                            Some(Instant::now() + self.config.timing.connect_timeout);
                    }
                }
            }
            if self.frame.is_none()
                && let Some(group) = &mut self.group
                && let Poll::Ready(result) = group.poll_next_frame(waiter)
            {
                let frame = result
                    .map_err(transport)?
                    .ok_or(Error::Invalid("empty state group"))?;
                if frame.size > tidkod_protocol::MAX_MESSAGE as u64 {
                    return Err(tidkod_protocol::Error::Size.into());
                }
                self.frame = Some(frame);
            }
            if let Some(frame) = &mut self.frame
                && let Poll::Ready(result) = frame.poll_read_all(waiter)
            {
                let bytes = result.map_err(transport)?;
                let state = Timeline::from_wire(&decode_snapshot(&bytes)?)?;
                if let Some(e) = self.core.state(state, self.clock.now_ns()) {
                    emit(&self.events, Event::Correction(e));
                }
                self.frame = None;
                self.group = None;
                self.frame_deadline = None;
            }
            if self.frame_deadline.is_some_and(|d| d <= Instant::now()) {
                return Err(transport("snapshot receive timeout"));
            }
            if Instant::now() >= self.next_probe {
                let timestamp = stamp(self.clock);
                let payload = self.probes.request(self.clock.now_ns())?;
                peer.datagrams
                    .append_datagram(timestamp, payload)
                    .map_err(transport)?;
                self.io.flush(cx)?;
                self.probes.publication_finished(self.clock.now_ns());
                if self.config.clock_diagnostics {
                    emit(
                        &self.events,
                        Event::ProbeTiming {
                            lateness_ns: Instant::now()
                                .saturating_duration_since(self.next_probe)
                                .as_nanos()
                                .min(u64::MAX as u128)
                                as u64,
                        },
                    );
                }
                let stats = peer.session.as_ref().unwrap().stats();
                self.core.view.rtt_ns = stats.rtt.map_or(0, |d| d.as_nanos() as u64);
                self.core.view.lost_packets = stats.packets_lost.unwrap_or(0);
                self.next_probe = Instant::now()
                    + if self.core.needs_fast_probes(self.clock.now_ns()) {
                        self.config.timing.acquisition_probe
                    } else {
                        self.config.timing.steady_probe
                    };
            }
        }
        Ok(())
    }
}
impl Job for FollowerJob {
    fn step(&mut self, cx: &mut Context<'_>) -> bool {
        if self.stop.load(Ordering::Acquire) {
            return false;
        }
        if let Err(e) = self.progress(cx) {
            emit(&self.events, Event::Error(e.to_string()));
            self.disconnect();
        }
        if let Some(e) = self.core.tick(
            self.clock.now_ns(),
            (self.config.timing.steady_probe.as_nanos() * 8) as u64,
        ) {
            emit(&self.events, Event::Correction(e));
        }
        self.readers.publish(self.core.view);
        true
    }
    fn deadline(&self) -> Option<Instant> {
        let mut deadline = self
            .io
            .deadline()
            .into_iter()
            .chain(self.peer.as_ref().and_then(|p| p.driver_deadline))
            .min();
        let connection = if let Some(peer) = &self.peer {
            if self.connected {
                self.next_probe
            } else {
                peer.deadline
            }
        } else {
            self.retry
        };
        deadline = Some(deadline.map_or(connection, |d| d.min(connection)));
        if let Some(d) = self.frame_deadline {
            deadline = Some(deadline.unwrap().min(d));
        }
        let view = self.core.view;
        if view.timeline.scheduled_len > 0 && view.mapping.last_sample_ns > 0 {
            let delta = view.timeline.scheduled[0]
                .anchor
                .time_ns
                .saturating_sub(view.mapping.leader_time(self.clock.now_ns()));
            let delay = (delta as f64 / (1. + view.mapping.drift)).max(0.) as u64;
            deadline = Some(
                deadline
                    .unwrap()
                    .min(Instant::now() + Duration::from_nanos(delay)),
            );
        }
        deadline
    }
    fn ready(&self) -> bool {
        self.command_more || self.io.endpoint.needs_pass() || self.io.runtime.needs_pass()
    }
}
impl Drop for FollowerJob {
    fn drop(&mut self) {
        self.core.disconnected();
        self.core.view.connection = ConnectionState::Shutdown;
        self.readers.publish(self.core.view);
    }
}
fn change_timeline(
    t: &mut Timeline,
    change: Change,
    effective: Option<u64>,
    now: u64,
) -> Result<()> {
    let time = effective.unwrap_or(now);
    if time > i64::MAX as u64 {
        return Err(Error::Invalid("scheduled timestamp"));
    }
    let scheduled = time > now;
    if scheduled
        && (t.scheduled_len == 4
            || (t.scheduled_len > 0 && time <= t.scheduled[t.scheduled_len - 1].anchor.time_ns))
    {
        return Err(Error::Invalid("schedule full or non-increasing"));
    }
    let (position, rate, disc) = t.at(time);
    let (position, rate) = match change {
        Change::Rate(r) => (position, r),
        Change::Seek(p) => (p, rate),
        Change::Set(p, r) => (p, r),
    };
    let anchor = Anchor {
        time_ns: time,
        position,
        rate,
    };
    // Immediate controls cancel pending controls and use a fresh discontinuity above all reserved IDs.
    let next = t.discontinuity.max(disc).max(
        t.scheduled[..t.scheduled_len]
            .last()
            .map_or(0, |s| s.discontinuity),
    ) + 1;
    if scheduled {
        t.scheduled[t.scheduled_len] = Scheduled {
            discontinuity: next,
            anchor,
        };
        t.scheduled_len += 1;
    } else {
        t.anchor = anchor;
        t.discontinuity = next;
        t.scheduled_len = 0;
    }
    Ok(())
}
impl Drop for Peer {
    fn drop(&mut self) {
        if let Some(pending) = &self.tls {
            pending.close();
        }
        if let Some(connection) = &self.connection {
            connection.close_code(
                if connection.protocol() == Some("h3") {
                    0x100
                } else {
                    0
                },
                "peer released",
            );
        }
    }
}
