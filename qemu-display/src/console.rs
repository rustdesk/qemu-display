use std::convert::TryFrom;
use std::os::unix::net::UnixStream;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Mutex;
use std::{os::unix::io::AsRawFd, thread};

use zbus::{
    dbus_proxy,
    zvariant::{Fd, ObjectPath},
};

use crate::Result;
use crate::{AsyncKeyboardProxy, AsyncMouseProxy, ConsoleEvent, ConsoleListener};

#[dbus_proxy(default_service = "org.qemu", interface = "org.qemu.Display1.Console")]
pub trait Console {
    /// RegisterListener method
    fn register_listener(&self, listener: Fd) -> zbus::Result<()>;

    /// SetUIInfo method
    #[dbus_proxy(name = "SetUIInfo")]
    fn set_ui_info(
        &self,
        width_mm: u16,
        height_mm: u16,
        xoff: i32,
        yoff: i32,
        width: u32,
        height: u32,
    ) -> zbus::Result<()>;

    #[dbus_proxy(property)]
    fn label(&self) -> zbus::Result<String>;

    #[dbus_proxy(property)]
    fn head(&self) -> zbus::Result<u32>;

    #[dbus_proxy(property)]
    fn type_(&self) -> zbus::Result<String>;

    #[dbus_proxy(property)]
    fn width(&self) -> zbus::Result<u32>;

    #[dbus_proxy(property)]
    fn height(&self) -> zbus::Result<u32>;
}

#[derive(derivative::Derivative)]
#[derivative(Debug)]
pub struct Console {
    #[derivative(Debug = "ignore")]
    pub proxy: AsyncConsoleProxy<'static>,
    #[derivative(Debug = "ignore")]
    pub keyboard: AsyncKeyboardProxy<'static>,
    #[derivative(Debug = "ignore")]
    pub mouse: AsyncMouseProxy<'static>,
}

impl Console {
    pub async fn new(conn: &zbus::azync::Connection, idx: u32) -> Result<Self> {
        let obj_path = ObjectPath::try_from(format!("/org/qemu/Display1/Console_{}", idx))?;
        let proxy = AsyncConsoleProxy::builder(conn)
            .path(&obj_path)?
            .build()
            .await?;
        let keyboard = AsyncKeyboardProxy::builder(conn)
            .path(&obj_path)?
            .build()
            .await?;
        let mouse = AsyncMouseProxy::builder(conn)
            .path(&obj_path)?
            .build()
            .await?;
        Ok(Self {
            proxy,
            keyboard,
            mouse,
        })
    }

    pub async fn dispatch_signals(&self) -> Result<()> {
        use futures_util::{future::FutureExt, select};

        if let Some(msg) = select!(
            msg = self.proxy.next_signal().fuse() => {
                msg?
            },
            msg = self.keyboard.next_signal().fuse() => {
                msg?
            },
            msg = self.mouse.next_signal().fuse() => {
                msg?
            }
        ) {
            if msg.primary_header().msg_type() == zbus::MessageType::Signal {
                log::debug!("Ignoring {:?}", msg);
            }
        }
        Ok(())
    }

    pub async fn label(&self) -> Result<String> {
        Ok(self.proxy.label().await?)
    }

    pub async fn width(&self) -> Result<u32> {
        Ok(self.proxy.width().await?)
    }

    pub async fn height(&self) -> Result<u32> {
        Ok(self.proxy.height().await?)
    }

    pub async fn listen(&self) -> Result<(Receiver<ConsoleEvent>, Sender<()>)> {
        let (p0, p1) = UnixStream::pair()?;
        let (tx, rx) = mpsc::channel();
        self.proxy.register_listener(p0.as_raw_fd().into()).await?;

        let (wait_tx, wait_rx) = mpsc::channel();
        let _thread = thread::spawn(move || {
            let c = zbus::ConnectionBuilder::unix_stream(p1)
                .p2p()
                .build()
                .unwrap();
            let mut s = zbus::ObjectServer::new(&c);
            let listener = ConsoleListener::new(Mutex::new(tx), wait_rx);
            let err = listener.err();
            s.at("/org/qemu/Display1/Listener", listener).unwrap();
            loop {
                if let Err(e) = s.try_handle_next() {
                    eprintln!("Listener DBus error: {}", e);
                    return;
                }
                if let Some(e) = err.get() {
                    eprintln!("Listener channel error: {}", e);
                    return;
                }
            }
        });

        Ok((rx, wait_tx))
    }
}

#[cfg(feature = "glib")]
impl Console {
    pub async fn glib_listen(&self) -> Result<(glib::Receiver<ConsoleEvent>, Sender<()>)> {
        let (p0, p1) = UnixStream::pair()?;
        let (tx, rx) = glib::MainContext::channel(glib::source::Priority::default());
        self.proxy.register_listener(p0.as_raw_fd().into()).await?;

        let (wait_tx, wait_rx) = mpsc::channel();
        let _thread = thread::spawn(move || {
            let c = zbus::ConnectionBuilder::unix_stream(p1)
                .p2p()
                .build()
                .unwrap();
            let mut s = zbus::ObjectServer::new(&c);
            let listener = ConsoleListener::new(tx, wait_rx);
            let err = listener.err();
            s.at("/org/qemu/Display1/Listener", listener).unwrap();
            loop {
                if let Err(e) = s.try_handle_next() {
                    eprintln!("Listener DBus error: {}", e);
                    break;
                }
                if let Some(e) = err.get() {
                    eprintln!("Listener channel error: {}", e);
                    break;
                }
            }
        });

        Ok((rx, wait_tx))
    }
}