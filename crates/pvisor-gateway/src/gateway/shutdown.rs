//! Stop transport I/O as well as handlers, including clients that stop reading.
use axum::serve::Listener;
use std::{
    future::Future,
    io,
    net::SocketAddr,
    pin::Pin,
    task::{Context, Poll},
};
use tokio::{
    io::{AsyncRead, AsyncWrite, ReadBuf},
    net::{TcpListener, TcpStream},
    sync::watch,
};

pub(super) struct StopListener {
    listener: TcpListener,
    stop: watch::Receiver<()>,
}
impl StopListener {
    pub(super) fn new(listener: TcpListener, stop: watch::Receiver<()>) -> Self {
        Self { listener, stop }
    }
}
impl Listener for StopListener {
    type Io = StopStream;
    type Addr = SocketAddr;
    async fn accept(&mut self) -> (Self::Io, Self::Addr) {
        let (stream, address) = Listener::accept(&mut self.listener).await;
        let mut stop = self.stop.clone();
        (
            StopStream {
                stream,
                stopped: false,
                stop: Box::pin(async move {
                    let _ = stop.changed().await;
                    // Existing model handlers can return their cancellation response first.
                    tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                }),
            },
            address,
        )
    }
    fn local_addr(&self) -> io::Result<Self::Addr> {
        self.listener.local_addr()
    }
}

pub(super) struct StopStream {
    stream: TcpStream,
    stop: Pin<Box<dyn Future<Output = ()> + Send>>,
    stopped: bool,
}
impl StopStream {
    fn check(&mut self, cx: &mut Context<'_>) -> io::Result<()> {
        self.stopped = self.stopped || self.stop.as_mut().poll(cx).is_ready();
        if self.stopped {
            Err(io::ErrorKind::ConnectionAborted.into())
        } else {
            Ok(())
        }
    }
}
impl AsyncRead for StopStream {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        self.check(cx)?;
        Pin::new(&mut self.stream).poll_read(cx, buf)
    }
}
impl AsyncWrite for StopStream {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        self.check(cx)?;
        Pin::new(&mut self.stream).poll_write(cx, buf)
    }
    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.check(cx)?;
        Pin::new(&mut self.stream).poll_flush(cx)
    }
    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.stream).poll_shutdown(cx)
    }
    fn is_write_vectored(&self) -> bool {
        self.stream.is_write_vectored()
    }
    fn poll_write_vectored(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bufs: &[io::IoSlice<'_>],
    ) -> Poll<io::Result<usize>> {
        self.check(cx)?;
        Pin::new(&mut self.stream).poll_write_vectored(cx, bufs)
    }
}
