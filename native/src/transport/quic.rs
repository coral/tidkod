use crate::transport::Error;
use bytes::{Bytes, BytesMut};
use quinn_proto::{ConnectionHandle, DatagramEvent, Dir, StreamId, VarInt};
use std::{
    cell::{Cell, RefCell},
    collections::{HashMap, VecDeque},
    io::{self, IoSliceMut},
    net::SocketAddr,
    rc::Rc,
    sync::Arc,
    task::{Context, Poll, Waker},
    time::Instant,
};
use web_transport_trait::poll::{RecvStream as _, SendStream as _};

const OPERATIONS: usize = 8;
struct Shared {
    proto: RefCell<quinn_proto::Connection>,
    error: RefCell<Option<Error>>,
    waiters: RefCell<Vec<Waker>>,
    budget: Cell<usize>,
    needs_pass: Cell<bool>,
    connected: Cell<bool>,
    finished: RefCell<HashMap<StreamId, Option<Result<(), Error>>>>,
}
impl Shared {
    fn new(proto: quinn_proto::Connection) -> Rc<Self> {
        Rc::new(Self {
            proto: RefCell::new(proto),
            error: RefCell::new(None),
            waiters: RefCell::new(Vec::new()),
            budget: Cell::new(OPERATIONS),
            needs_pass: Cell::new(true),
            connected: Cell::new(false),
            finished: RefCell::new(HashMap::new()),
        })
    }
    fn park(&self, cx: &Context<'_>) {
        let mut w = self.waiters.borrow_mut();
        if !w.iter().any(|w| w.will_wake(cx.waker())) {
            w.push(cx.waker().clone());
        }
    }
    fn wake(&self) {
        for w in self.waiters.take() {
            w.wake();
        }
    }
    fn enter(&self, cx: &Context<'_>) -> Poll<Result<(), Error>> {
        if let Some(e) = self.error.borrow().as_ref() {
            return Poll::Ready(Err(e.clone()));
        }
        if self.budget.get() == 0 {
            self.needs_pass.set(true);
            self.park(cx);
            return Poll::Pending;
        }
        Poll::Ready(Ok(()))
    }
    fn consumed(&self) {
        self.budget.set(self.budget.get().saturating_sub(1));
    }
    fn changed(&self) {
        self.needs_pass.set(true);
    }
}
/// A pending TLS handshake, driven by its endpoint.
pub struct PendingConnection(Rc<Shared>);
impl PendingConnection {
    pub fn close(&self) {
        Connection {
            shared: self.0.clone(),
            alpn: None,
        }
        .close_code(0, "cancelled");
    }
    pub fn ready(&self) -> Result<Option<Connection>, Error> {
        if let Some(e) = self.0.error.borrow().as_ref() {
            return Err(e.clone());
        }
        if !self.0.connected.get() {
            return Ok(None);
        }
        let alpn = self
            .0
            .proto
            .borrow()
            .crypto_session()
            .handshake_data()
            .and_then(|d| {
                d.downcast::<quinn_proto::crypto::rustls::HandshakeData>()
                    .ok()
            })
            .and_then(|d| d.protocol)
            .and_then(|p| String::from_utf8(p).ok());
        Ok(Some(Connection {
            shared: self.0.clone(),
            alpn,
        }))
    }
}
#[derive(Clone)]
pub struct Connection {
    shared: Rc<Shared>,
    alpn: Option<String>,
}
impl Connection {
    pub fn close_code(&self, code: u64, reason: &str) {
        if self.shared.error.borrow().is_some() {
            return;
        }
        self.shared.proto.borrow_mut().close(
            Instant::now(),
            VarInt::from_u64(code).unwrap_or(VarInt::MAX),
            Bytes::copy_from_slice(reason.as_bytes()),
        );
        *self.shared.error.borrow_mut() = Some(Error::Closed {
            code,
            reason: reason.into(),
        });
        self.shared.changed();
        self.shared.wake();
    }
    pub fn peer_certificates(&self) -> Option<Vec<Vec<u8>>> {
        let id = self
            .shared
            .proto
            .borrow()
            .crypto_session()
            .peer_identity()?;
        Some(
            id.downcast::<Vec<rustls::pki_types::CertificateDer<'static>>>()
                .ok()?
                .iter()
                .map(|x| x.to_vec())
                .collect(),
        )
    }
    fn streams(
        &self,
        cx: &Context<'_>,
        direction: Dir,
        open: bool,
    ) -> Poll<Result<StreamId, Error>> {
        std::task::ready!(self.shared.enter(cx))?;
        let id = {
            let mut c = self.shared.proto.borrow_mut();
            if open {
                c.streams().open(direction)
            } else {
                c.streams().accept(direction)
            }
        };
        if let Some(id) = id {
            self.shared.consumed();
            self.shared.changed();
            Poll::Ready(Ok(id))
        } else {
            self.shared.park(cx);
            Poll::Pending
        }
    }
}
impl web_transport_trait::poll::Session for Connection {
    type SendStream = SendStream;
    type RecvStream = RecvStream;
    type Error = Error;
    fn poll_accept_uni(&mut self, cx: &mut Context<'_>) -> Poll<Result<RecvStream, Error>> {
        let id = std::task::ready!(self.streams(cx, Dir::Uni, false))?;
        Poll::Ready(Ok(RecvStream::new(self.shared.clone(), id)))
    }
    fn poll_accept_bi(
        &mut self,
        cx: &mut Context<'_>,
    ) -> Poll<Result<(SendStream, RecvStream), Error>> {
        let id = std::task::ready!(self.streams(cx, Dir::Bi, false))?;
        Poll::Ready(Ok((
            SendStream::new(self.shared.clone(), id),
            RecvStream::new(self.shared.clone(), id),
        )))
    }
    fn poll_open_uni(&mut self, cx: &mut Context<'_>) -> Poll<Result<SendStream, Error>> {
        let id = std::task::ready!(self.streams(cx, Dir::Uni, true))?;
        Poll::Ready(Ok(SendStream::new(self.shared.clone(), id)))
    }
    fn poll_open_bi(
        &mut self,
        cx: &mut Context<'_>,
    ) -> Poll<Result<(SendStream, RecvStream), Error>> {
        let id = std::task::ready!(self.streams(cx, Dir::Bi, true))?;
        Poll::Ready(Ok((
            SendStream::new(self.shared.clone(), id),
            RecvStream::new(self.shared.clone(), id),
        )))
    }
    fn poll_send_datagram(
        &mut self,
        cx: &mut Context<'_>,
        payload: &[u8],
    ) -> Poll<Result<(), Error>> {
        std::task::ready!(self.shared.enter(cx))?;
        let result = self
            .shared
            .proto
            .borrow_mut()
            .datagrams()
            .send(Bytes::copy_from_slice(payload), false);
        match result {
            Ok(()) => {
                self.shared.consumed();
                self.shared.changed();
                Poll::Ready(Ok(()))
            }
            Err(quinn_proto::SendDatagramError::Blocked(_)) => {
                self.shared.park(cx);
                Poll::Pending
            }
            Err(e) => Poll::Ready(Err(Error::quic(e))),
        }
    }
    fn poll_recv_datagram(&mut self, cx: &mut Context<'_>) -> Poll<Result<Bytes, Error>> {
        std::task::ready!(self.shared.enter(cx))?;
        if let Some(b) = self.shared.proto.borrow_mut().datagrams().recv() {
            self.shared.consumed();
            Poll::Ready(Ok(b))
        } else {
            self.shared.park(cx);
            Poll::Pending
        }
    }
    fn max_datagram_size(&self) -> usize {
        self.shared
            .proto
            .borrow_mut()
            .datagrams()
            .max_size()
            .unwrap_or(0)
    }
    fn protocol(&self) -> Option<&str> {
        self.alpn.as_deref()
    }
    fn close(&mut self, code: u32, reason: &str) {
        self.close_code(code.into(), reason);
    }
    fn poll_closed(&mut self, cx: &mut Context<'_>) -> Poll<Error> {
        if let Some(e) = self.shared.error.borrow().as_ref() {
            Poll::Ready(e.clone())
        } else {
            self.shared.park(cx);
            Poll::Pending
        }
    }
    fn stats(&self) -> impl web_transport_trait::Stats {
        Stats(self.shared.proto.borrow().stats())
    }
}
struct Stats(quinn_proto::ConnectionStats);
impl web_transport_trait::Stats for Stats {
    fn rtt(&self) -> Option<std::time::Duration> {
        Some(self.0.path.rtt)
    }
    fn packets_lost(&self) -> Option<u64> {
        Some(self.0.path.lost_packets)
    }
    fn packets_sent(&self) -> Option<u64> {
        Some(self.0.udp_tx.datagrams)
    }
    fn packets_received(&self) -> Option<u64> {
        Some(self.0.udp_rx.datagrams)
    }
    fn bytes_sent(&self) -> Option<u64> {
        Some(self.0.udp_tx.bytes)
    }
    fn bytes_received(&self) -> Option<u64> {
        Some(self.0.udp_rx.bytes)
    }
}
pub struct SendStream {
    shared: Rc<Shared>,
    id: StreamId,
    ended: bool,
}
impl SendStream {
    fn new(shared: Rc<Shared>, id: StreamId) -> Self {
        shared.finished.borrow_mut().insert(id, None);
        Self {
            shared,
            id,
            ended: false,
        }
    }
    pub fn id(&self) -> u64 {
        self.id.into()
    }
    pub fn ended(&self) -> bool {
        self.ended
    }
    pub fn reset_code(&mut self, code: u64) {
        let _ = self
            .shared
            .proto
            .borrow_mut()
            .send_stream(self.id)
            .reset(VarInt::from_u64(code).unwrap_or(VarInt::MAX));
        self.ended = true;
        self.shared.changed();
    }
    pub fn try_write(&mut self, buf: &[u8]) -> usize {
        if self.ended {
            return 0;
        }
        let n = self
            .shared
            .proto
            .borrow_mut()
            .send_stream(self.id)
            .write(buf)
            .unwrap_or(0);
        self.shared.changed();
        n
    }
}
impl web_transport_trait::poll::SendStream for SendStream {
    type Error = Error;
    fn poll_write(&mut self, cx: &mut Context<'_>, buf: &[u8]) -> Poll<Result<usize, Error>> {
        std::task::ready!(self.shared.enter(cx))?;
        match self
            .shared
            .proto
            .borrow_mut()
            .send_stream(self.id)
            .write(buf)
        {
            Ok(n) => {
                self.shared.consumed();
                self.shared.changed();
                Poll::Ready(Ok(n))
            }
            Err(quinn_proto::WriteError::Blocked) => {
                self.shared.park(cx);
                Poll::Pending
            }
            Err(quinn_proto::WriteError::Stopped(c)) => {
                Poll::Ready(Err(Error::Reset(c.into_inner())))
            }
            Err(e) => Poll::Ready(Err(Error::quic(e))),
        }
    }
    fn set_priority(&mut self, order: u8) {
        let _ = self
            .shared
            .proto
            .borrow_mut()
            .send_stream(self.id)
            .set_priority(order.into());
    }
    fn finish(&mut self) -> Result<(), Error> {
        if !self.ended {
            self.shared
                .proto
                .borrow_mut()
                .send_stream(self.id)
                .finish()
                .map_err(Error::quic)?;
            self.ended = true;
            self.shared.changed();
        }
        Ok(())
    }
    fn reset(&mut self, code: u32) {
        self.reset_code(code.into());
    }
    fn poll_closed(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Error>> {
        if let Some(Some(e)) = self.shared.finished.borrow().get(&self.id) {
            return Poll::Ready(e.clone());
        }
        if let Some(e) = self.shared.error.borrow().as_ref() {
            return Poll::Ready(Err(e.clone()));
        }
        self.shared.park(cx);
        Poll::Pending
    }
}
impl Drop for SendStream {
    fn drop(&mut self) {
        if !self.ended {
            self.reset(0);
        }
        self.shared.finished.borrow_mut().remove(&self.id);
    }
}
pub struct RecvStream {
    shared: Rc<Shared>,
    id: StreamId,
    ended: bool,
    backlog: Bytes,
}
impl RecvStream {
    fn new(shared: Rc<Shared>, id: StreamId) -> Self {
        Self {
            shared,
            id,
            ended: false,
            backlog: Bytes::new(),
        }
    }
    pub fn id(&self) -> u64 {
        self.id.into()
    }
    pub fn stop_code(&mut self, code: u64) {
        let _ = self
            .shared
            .proto
            .borrow_mut()
            .recv_stream(self.id)
            .stop(VarInt::from_u64(code).unwrap_or(VarInt::MAX));
        self.ended = true;
        self.backlog = Bytes::new();
        self.shared.changed();
    }
    fn read(&mut self, cx: &Context<'_>, max: usize) -> Poll<Result<Option<Bytes>, Error>> {
        if self.ended {
            return Poll::Ready(Ok(None));
        }
        std::task::ready!(self.shared.enter(cx))?;
        let mut c = self.shared.proto.borrow_mut();
        let mut stream = c.recv_stream(self.id);
        let mut chunks = match stream.read(true) {
            Ok(c) => c,
            Err(e) => return Poll::Ready(Err(Error::quic(e))),
        };
        let result = chunks.next(max);
        let transmit = chunks.finalize();
        drop(c);
        if transmit.should_transmit() {
            self.shared.changed();
        }
        match result {
            Ok(Some(chunk)) => {
                self.shared.consumed();
                Poll::Ready(Ok(Some(chunk.bytes)))
            }
            Ok(None) => {
                self.ended = true;
                Poll::Ready(Ok(None))
            }
            Err(quinn_proto::ReadError::Blocked) => {
                self.shared.park(cx);
                Poll::Pending
            }
            Err(quinn_proto::ReadError::Reset(code)) => {
                Poll::Ready(Err(Error::Reset(code.into_inner())))
            }
        }
    }
}
impl web_transport_trait::poll::RecvStream for RecvStream {
    type Error = Error;
    fn poll_read(
        &mut self,
        cx: &mut Context<'_>,
        dst: &mut [u8],
    ) -> Poll<Result<Option<usize>, Error>> {
        if dst.is_empty() {
            return Poll::Ready(Ok(Some(0)));
        }
        if self.backlog.is_empty() {
            match std::task::ready!(self.read(cx, dst.len()))? {
                Some(b) => self.backlog = b,
                None => return Poll::Ready(Ok(None)),
            }
        }
        let n = dst.len().min(self.backlog.len());
        dst[..n].copy_from_slice(&self.backlog.split_to(n));
        if self.backlog.is_empty() {
            self.shared.wake();
        }
        Poll::Ready(Ok(Some(n)))
    }
    fn stop(&mut self, code: u32) {
        self.stop_code(code.into());
    }
    fn poll_closed(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Error>> {
        if self.ended {
            return Poll::Ready(Ok(()));
        }
        if !self.backlog.is_empty() {
            self.shared.park(cx);
            return Poll::Pending;
        }
        match std::task::ready!(self.read(cx, 65536))? {
            None => Poll::Ready(Ok(())),
            Some(b) => {
                self.backlog = b;
                self.shared.park(cx);
                Poll::Pending
            }
        }
    }
}
impl Drop for RecvStream {
    fn drop(&mut self) {
        if !self.ended {
            self.stop(0);
        }
    }
}

/// A UDP socket and QUIC routing table. The owner must call `step` after readiness,
/// on deadlines, and after application writes. Calls are bounded and never wait.
pub struct Endpoint {
    socket: mio::net::UdpSocket,
    udp: quinn_udp::UdpSocketState,
    proto: quinn_proto::Endpoint,
    connections: HashMap<ConnectionHandle, Rc<Shared>>,
    incoming: VecDeque<PendingConnection>,
    out: VecDeque<(quinn_proto::Transmit, Vec<u8>)>,
    buffer: Vec<u8>,
    limit: usize,
    next: usize,
    receive_pending: Option<(quinn_udp::RecvMeta, usize)>,
    receive_more: bool,
    order: Vec<ConnectionHandle>,
    send_buffers: Vec<Vec<u8>>,
    write_blocked: bool,
    registration: Option<(mio::Registry, mio::Token)>,
}
impl Endpoint {
    pub fn new(
        bind: SocketAddr,
        server: Option<quinn_proto::ServerConfig>,
        limit: usize,
    ) -> io::Result<Self> {
        let socket = std::net::UdpSocket::bind(bind)?;
        socket.set_nonblocking(true)?;
        let udp = quinn_udp::UdpSocketState::new((&socket).into())?;
        let socket = mio::net::UdpSocket::from_std(socket);
        let proto = quinn_proto::Endpoint::new(
            Arc::new(quinn_proto::EndpointConfig::default()),
            server.map(Arc::new),
            true,
            None,
        );
        Ok(Self {
            socket,
            udp,
            proto,
            connections: HashMap::new(),
            incoming: VecDeque::new(),
            out: VecDeque::new(),
            buffer: vec![0; 65536],
            limit,
            next: 0,
            receive_pending: None,
            receive_more: false,
            order: Vec::new(),
            send_buffers: Vec::new(),
            write_blocked: false,
            registration: None,
        })
    }
    pub fn register(&mut self, registry: &mio::Registry, token: mio::Token) -> io::Result<()> {
        let owned_registry = registry.try_clone()?;
        let interest = self.interest();
        registry.register(&mut self.socket, token, interest)?;
        self.registration = Some((owned_registry, token));
        Ok(())
    }
    fn interest(&self) -> mio::Interest {
        // On Windows, try_io re-arms all registered interests after WouldBlock.
        // A normally writable UDP socket would otherwise keep waking idle workers.
        if self.write_blocked {
            mio::Interest::READABLE.add(mio::Interest::WRITABLE)
        } else {
            mio::Interest::READABLE
        }
    }
    fn set_write_blocked(&mut self, blocked: bool) -> io::Result<()> {
        if self.write_blocked != blocked {
            self.write_blocked = blocked;
            if let Some((registry, token)) = &self.registration {
                let interest = self.interest();
                registry.reregister(&mut self.socket, *token, interest)?;
            }
        }
        Ok(())
    }
    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        self.socket.local_addr()
    }
    pub fn connect(
        &mut self,
        remote: SocketAddr,
        pin: Option<&str>,
    ) -> Result<PendingConnection, Error> {
        if self.connections.len() >= self.limit {
            return Err(Error::Invalid("connection capacity"));
        }
        let (id, c) = self
            .proto
            .connect(
                Instant::now(),
                crate::transport::tls::client(pin)?,
                remote,
                "localhost",
            )
            .map_err(Error::quic)?;
        let shared = Shared::new(c);
        self.connections.insert(id, shared.clone());
        Ok(PendingConnection(shared))
    }
    pub fn accept(&mut self) -> Option<PendingConnection> {
        self.incoming.pop_front()
    }
    pub fn next_deadline(&self) -> Option<Instant> {
        self.connections
            .values()
            .filter_map(|c| c.proto.borrow_mut().poll_timeout())
            .min()
    }
    pub fn needs_pass(&self) -> bool {
        self.receive_more || self.connections.values().any(|c| c.needs_pass.get())
    }
    /// Receive at most 32 datagrams; each connection gets eight QUIC sends/events
    /// and eight transport operations before the next owner pass.
    pub fn step(&mut self, now: Instant) -> io::Result<()> {
        self.receive_more = true;
        for _ in 0..32 {
            if self.receive_pending.is_none() {
                let mut meta = [quinn_udp::RecvMeta::default()];
                // Quinn uses the raw socket. Mio must observe WouldBlock to
                // re-arm readiness notifications, particularly on Windows.
                let n = match self.socket.try_io(|| {
                    self.udp.recv(
                        (&self.socket).into(),
                        &mut [IoSliceMut::new(&mut self.buffer)],
                        &mut meta,
                    )
                }) {
                    Ok(n) => n,
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                        self.receive_more = false;
                        break;
                    }
                    Err(e) => return Err(e),
                };
                if n == 0 {
                    self.receive_more = false;
                    break;
                }
                self.receive_pending = Some((meta[0], 0));
            }
            let (m, start) = self.receive_pending.unwrap();
            let end = (start + m.stride.max(1)).min(m.len);
            self.receive_pending = if end < m.len { Some((m, end)) } else { None };
            let segment = &self.buffer[start..end];
            let mut response = Vec::new();
            let event = self.proto.handle(
                now,
                m.addr,
                m.dst_ip,
                m.ecn
                    .and_then(|e| quinn_proto::EcnCodepoint::from_bits(e as u8)),
                BytesMut::from(segment),
                &mut response,
            );
            match event {
                Some(DatagramEvent::ConnectionEvent(id, e)) => {
                    if let Some(c) = self.connections.get(&id) {
                        c.proto.borrow_mut().handle_event(e);
                        c.wake();
                    }
                }
                Some(DatagramEvent::NewConnection(incoming)) => {
                    if self.connections.len() >= self.limit {
                        self.proto.ignore(incoming);
                        continue;
                    }
                    match self.proto.accept(incoming, now, &mut response, None) {
                        Ok((id, c)) => {
                            let c = Shared::new(c);
                            self.connections.insert(id, c.clone());
                            self.incoming.push_back(PendingConnection(c));
                        }
                        Err(e) => {
                            if let Some(t) = e.response
                                && self.out.len() < 64
                            {
                                self.out.push_back((t, response));
                            }
                        }
                    }
                }
                Some(DatagramEvent::Response(t)) if self.out.len() < 64 => {
                    self.out.push_back((t, response));
                }
                None | Some(DatagramEvent::Response(_)) => {}
            }
        }
        self.drive(now)
    }
    /// Run connection transmit/event work and flush queued UDP without receiving. Owners call
    /// this after queueing latency-sensitive datagrams instead of waiting for the next pass.
    pub fn drive(&mut self, now: Instant) -> io::Result<()> {
        let mut ids = std::mem::take(&mut self.order);
        ids.clear();
        ids.extend(self.connections.keys().copied());
        let len = ids.len();
        if len > 0 {
            ids.rotate_left(self.next % len);
            self.next = (self.next + 1) % len;
        }
        for id in ids.iter().copied() {
            let c = self.connections[&id].clone();
            let was_ready = c.needs_pass.replace(false);
            c.budget.set(OPERATIONS);
            if was_ready {
                c.wake();
            }
            let mut proto = c.proto.borrow_mut();
            if proto.poll_timeout().is_some_and(|deadline| deadline <= now) {
                proto.handle_timeout(now);
                c.wake();
            }
            for operation in 0..8 {
                if self.out.len() >= 64 {
                    if !self.write_blocked {
                        c.needs_pass.set(true);
                    }
                    break;
                }
                let mut buffer = self.send_buffers.pop().unwrap_or_default();
                buffer.clear();
                if let Some(t) = proto.poll_transmit(now, 1, &mut buffer) {
                    self.out.push_back((t, buffer));
                    if operation == 7 {
                        c.needs_pass.set(true);
                    }
                } else {
                    self.send_buffers.push(buffer);
                    break;
                }
            }
            for operation in 0..8 {
                let Some(e) = proto.poll_endpoint_events() else {
                    break;
                };
                if let Some(e) = self.proto.handle_event(id, e) {
                    proto.handle_event(e);
                }
                if operation == 7 {
                    c.needs_pass.set(true);
                }
            }
            for operation in 0..8 {
                match proto.poll() {
                    Some(quinn_proto::Event::Connected) => {
                        c.connected.set(true);
                        c.wake();
                    }
                    Some(quinn_proto::Event::ConnectionLost { reason }) => {
                        *c.error.borrow_mut() = Some(match reason {
                            quinn_proto::ConnectionError::ApplicationClosed(reason) => {
                                Error::Closed {
                                    code: reason.error_code.into_inner(),
                                    reason: String::from_utf8_lossy(&reason.reason).into_owned(),
                                }
                            }
                            other => Error::quic(other),
                        });
                        c.wake();
                    }
                    Some(quinn_proto::Event::Stream(quinn_proto::StreamEvent::Finished { id })) => {
                        if let Some(slot) = c.finished.borrow_mut().get_mut(&id) {
                            *slot = Some(Ok(()));
                        }
                        c.wake();
                    }
                    Some(quinn_proto::Event::Stream(quinn_proto::StreamEvent::Stopped {
                        id,
                        error_code,
                    })) => {
                        if let Some(slot) = c.finished.borrow_mut().get_mut(&id) {
                            *slot = Some(Err(Error::Reset(error_code.into_inner())));
                        }
                        c.wake();
                    }
                    Some(_) => c.wake(),
                    None => break,
                }
                if operation == 7 {
                    c.needs_pass.set(true);
                }
            }
            if proto.is_drained() {
                drop(proto);
                self.connections.remove(&id);
            }
        }
        self.order = ids;
        while let Some((t, b)) = self.out.front() {
            let transmit = quinn_udp::Transmit {
                destination: t.destination,
                ecn: t
                    .ecn
                    .and_then(|e| quinn_udp::EcnCodepoint::from_bits(e as u8)),
                contents: &b[..t.size],
                segment_size: t.segment_size,
                src_ip: t.src_ip,
            };
            match self
                .socket
                .try_io(|| self.udp.send((&self.socket).into(), &transmit))
            {
                Ok(()) => {
                    if self.write_blocked {
                        self.set_write_blocked(false)?;
                        for c in self.connections.values() {
                            c.needs_pass.set(true);
                        }
                    }
                    let (_, mut buffer) = self.out.pop_front().unwrap();
                    buffer.clear();
                    if self.send_buffers.len() < 64 {
                        self.send_buffers.push(buffer);
                    }
                }
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                    self.set_write_blocked(true)?;
                    break;
                }
                Err(e) => return Err(e),
            }
        }
        Ok(())
    }
}

impl Drop for Endpoint {
    fn drop(&mut self) {
        for connection in self.connections.values() {
            connection.error.borrow_mut().get_or_insert(Error::Closed {
                code: 0,
                reason: "endpoint dropped".into(),
            });
            connection.wake();
        }
    }
}
impl RecvStream {
    pub fn ended(&self) -> bool {
        self.ended
    }
}
impl std::fmt::Debug for SendStream {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SendStream")
            .field("id", &self.id)
            .field("ended", &self.ended)
            .finish()
    }
}
impl std::fmt::Debug for RecvStream {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RecvStream")
            .field("id", &self.id)
            .field("ended", &self.ended)
            .finish()
    }
}
