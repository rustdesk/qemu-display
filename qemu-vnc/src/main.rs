use std::net::{TcpListener, TcpStream};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::{collections::HashSet, convert::TryInto};
use std::{error::Error, thread::JoinHandle};
use std::{io, thread, time};

use clap::Clap;
use image::GenericImage;
use qemu_display_listener::{Console, ConsoleEvent, MouseButton, VMProxy};
use keycodemap::*;
use vnc::{
    server::Event as VncEvent, server::FramebufferUpdate, Error as VncError, PixelFormat, Rect,
    Server as VncServer,
};
use zbus::Connection;

#[derive(Clap, Debug)]
pub struct SocketAddrArgs {
    /// IP address
    #[clap(short, long, default_value = "127.0.0.1")]
    address: std::net::IpAddr,
    /// IP port number
    #[clap(short, long, default_value = "5900")]
    port: u16,
}

impl From<SocketAddrArgs> for std::net::SocketAddr {
    fn from(args: SocketAddrArgs) -> Self {
        (args.address, args.port).into()
    }
}

#[derive(Clap, Debug)]
struct Cli {
    #[clap(flatten)]
    address: SocketAddrArgs,
    #[clap(short, long)]
    dbus_address: Option<String>,
}

#[derive(Debug)]
enum Event {
    ConsoleUpdate(Rect),
    Vnc(VncEvent),
    Disconnected,
}

const PIXMAN_X8R8G8B8: u32 = 0x20020888;
type BgraImage = image::ImageBuffer<image::Bgra<u8>, Vec<u8>>;

#[derive(derivative::Derivative)]
#[derivative(Debug)]
struct Client {
    #[derivative(Debug = "ignore")]
    server: Server,
    vnc_server: VncServer,
    share: bool,
    last_update: Option<time::Instant>,
    has_update: bool,
    req_update: bool,
    last_buttons: HashSet<MouseButton>,
}

impl Client {
    fn new(server: Server, vnc_server: VncServer, share: bool) -> Self {
        Self {
            server,
            vnc_server,
            share,
            last_update: None,
            has_update: false,
            req_update: false,
            last_buttons: HashSet::new(),
        }
    }

    fn update_pending(&self) -> bool {
        self.has_update && self.req_update
    }

    fn handle_vnc_event(&mut self, event: VncEvent) -> Result<(), Box<dyn Error>> {
        match event {
            VncEvent::FramebufferUpdateRequest { .. } => {
                self.req_update = true;
                self.send_framebuffer_update()?;
            }
            VncEvent::KeyEvent { key, down } => {
                let inner = self.server.inner.lock().unwrap();

                if let Some(qnum) = KEYMAP_X112QNUM.get(key as usize) {
                    if down {
                        inner.console.keyboard.press(*qnum as u32)?;
                    } else {
                        inner.console.keyboard.release(*qnum as u32)?;
                    }
                }
            }
            VncEvent::PointerEvent {
                button_mask,
                x_position,
                y_position,
            } => {
                let buttons = button_mask_to_set(button_mask);
                let inner = self.server.inner.lock().unwrap();

                for b in buttons.difference(&self.last_buttons) {
                    inner.console.mouse.press(*b)?;
                }
                for b in self.last_buttons.difference(&buttons) {
                    inner.console.mouse.release(*b)?;
                }
                inner
                    .console
                    .mouse
                    .set_abs_position(x_position as _, y_position as _)?;
                self.last_buttons = buttons;
            }
            // VncEvent::SetPixelFormat(_) => {}
            // VncEvent::SetEncodings(_) => {}
            // VncEvent::CutText(_) => {}
            // VncEvent::ExtendedKeyEvent { .. } => {}
            e => {
                dbg!(e);
            }
        }
        Ok(())
    }

    fn send_framebuffer_update(&mut self) -> Result<(), Box<dyn Error>> {
        if self.has_update && self.req_update {
            if let Some(last_update) = self.last_update {
                if last_update.elapsed().as_millis() < 10 {
                    println!("TODO: <10ms, could delay update..")
                }
            }
            self.server.send_framebuffer_update(&self.vnc_server)?;
            self.last_update = Some(time::Instant::now());
            self.has_update = false;
            self.req_update = false;
        }
        Ok(())
    }

    fn handle_event(&mut self, event: Option<Event>) -> Result<bool, Box<dyn Error>> {
        match event {
            Some(Event::Vnc(e)) => self.handle_vnc_event(e)?,
            Some(Event::ConsoleUpdate(_)) => {
                self.has_update = true;
            }
            Some(Event::Disconnected) => {
                return Ok(false);
            }
            None => {
                self.send_framebuffer_update()?;
            }
        }

        Ok(true)
    }
}

#[derive(Debug)]
struct ServerInner {
    console: Console,
    console_thread: Option<JoinHandle<()>>,
    image: BgraImage,
    tx: mpsc::Sender<Event>,
}

#[derive(Clone, Debug)]
struct Server {
    vm_name: String,
    rx: Arc<Mutex<mpsc::Receiver<Event>>>,
    inner: Arc<Mutex<ServerInner>>,
}

impl Server {
    fn new(vm_name: String, console: Console) -> Result<Self, Box<dyn Error>> {
        let (width, height) = (console.width()?, console.height()?);
        let image = BgraImage::new(width as _, height as _);
        let (tx, rx) = mpsc::channel();
        Ok(Self {
            vm_name,
            rx: Arc::new(Mutex::new(rx)),
            inner: Arc::new(Mutex::new(ServerInner {
                console,
                console_thread: None,
                image,
                tx,
            })),
        })
    }

    fn stop_console(&self) -> Result<(), Box<dyn Error>> {
        let mut inner = self.inner.lock().unwrap();
        if let Some(_thread) = inner.console_thread.take() {
            todo!("join console thread");
            //thread.join().unwrap();
        }
        Ok(())
    }

    fn run_console(&self) -> Result<(), Box<dyn Error>> {
        let mut inner = self.inner.lock().unwrap();
        if inner.console_thread.is_some() {
            return Ok(());
        }

        let server = self.clone();
        let (console_rx, _ack) = inner.console.listen()?;

        let thread = thread::spawn(move || loop {
            match console_rx.recv().unwrap() {
                ConsoleEvent::ScanoutDMABUF(_) | ConsoleEvent::UpdateDMABUF { .. } => {
                    unimplemented!();
                }
                ConsoleEvent::Scanout(s) => {
                    let mut inner = server.inner.lock().unwrap();
                    inner.image = image_from_vec(s.format, s.width, s.height, s.stride, s.data);
                }
                ConsoleEvent::Update(u) => {
                    let mut inner = server.inner.lock().unwrap();
                    let update = image_from_vec(
                        u.format,
                        u.w.try_into().unwrap(),
                        u.h.try_into().unwrap(),
                        u.stride,
                        u.data,
                    );
                    if (u.x, u.y) == (0, 0) && update.dimensions() == inner.image.dimensions() {
                        inner.image = update;
                    } else {
                        inner
                            .image
                            .copy_from(&update, u.x.try_into().unwrap(), u.y.try_into().unwrap())
                            .unwrap();
                    }
                    let rect = Rect {
                        left: u.x.try_into().unwrap(),
                        top: u.y.try_into().unwrap(),
                        width: u.w.try_into().unwrap(),
                        height: u.h.try_into().unwrap(),
                    };
                    inner.tx.send(Event::ConsoleUpdate(rect)).unwrap();
                }
                ConsoleEvent::CursorDefine { .. } => {}
                ConsoleEvent::MouseSet { .. } => {}
                e => {
                    dbg!(e);
                }
            }
        });

        inner.console_thread = Some(thread);
        Ok(())
    }

    fn dimensions(&self) -> (u16, u16) {
        let inner = self.inner.lock().unwrap();
        (inner.image.width() as u16, inner.image.height() as u16)
    }

    fn send_framebuffer_update(&self, server: &VncServer) -> Result<(), Box<dyn Error>> {
        let inner = self.inner.lock().unwrap();
        let mut fbu = FramebufferUpdate::new(&pixman_xrgb());
        let pixel_data = inner.image.as_raw();
        let rect = Rect {
            left: 0,
            top: 0,
            width: inner.image.width() as u16,
            height: inner.image.height() as u16,
        };
        fbu.add_raw_pixels(rect, &pixel_data);
        server.send(&fbu)?;
        Ok(())
    }

    fn handle_client(&self, stream: TcpStream) -> Result<(), Box<dyn Error>> {
        let (width, height) = self.dimensions();

        let (vnc_server, share) = VncServer::from_tcp_stream(
            stream,
            width,
            height,
            pixman_xrgb(),
            self.vm_name.clone(),
        )?;

        let tx = self.inner.lock().unwrap().tx.clone();
        let srv = vnc_server.clone();
        let _client_thread = thread::spawn(move || loop {
            let event = match srv.read_event() {
                Ok(e) => e,
                Err(VncError::Io(ref e)) if e.kind() == io::ErrorKind::WouldBlock => {
                    continue;
                }
                Err(VncError::Disconnected) => {
                    tx.send(Event::Disconnected).unwrap();
                    return;
                }
                Err(e) => {
                    eprintln!("Server read error: {}", e);
                    return;
                }
            };
            tx.send(Event::Vnc(event)).unwrap();
        });

        let mut client = Client::new(self.clone(), vnc_server, share);
        self.run_console()?;
        let rx = self.rx.lock().unwrap();
        loop {
            let ev = if client.update_pending() {
                match rx.try_recv() {
                    Ok(e) => Some(e),
                    Err(mpsc::TryRecvError::Empty) => None,
                    Err(e) => {
                        return Err(e.into());
                    }
                }
            } else {
                Some(rx.recv()?)
            };
            if !client.handle_event(ev)? {
                break;
            }
        }
        self.stop_console()?;
        Ok(())
    }
}

fn button_mask_to_set(mask: u8) -> HashSet<MouseButton> {
    let mut set = HashSet::new();
    if mask & 0b0000_0001 != 0 {
        set.insert(MouseButton::Left);
    }
    if mask & 0b0000_0010 != 0 {
        set.insert(MouseButton::Middle);
    }
    if mask & 0b0000_0100 != 0 {
        set.insert(MouseButton::Right);
    }
    if mask & 0b0000_1000 != 0 {
        set.insert(MouseButton::WheelUp);
    }
    if mask & 0b0001_0000 != 0 {
        set.insert(MouseButton::WheelDown);
    }
    set
}

pub fn pixman_xrgb() -> PixelFormat {
    PixelFormat {
        bits_per_pixel: 32,
        depth: 24,
        big_endian: true,
        true_colour: true,
        red_max: 255,
        green_max: 255,
        blue_max: 255,
        red_shift: 16,
        green_shift: 8,
        blue_shift: 0,
    }
}

fn image_from_vec(format: u32, width: u32, height: u32, stride: u32, data: Vec<u8>) -> BgraImage {
    if format != PIXMAN_X8R8G8B8 {
        todo!("unhandled pixman format: {}", format)
    }
    if cfg!(target_endian = "big") {
        todo!("pixman/image in big endian")
    }
    let layout = image::flat::SampleLayout {
        channels: 4,
        channel_stride: 1,
        width,
        width_stride: 4,
        height,
        height_stride: stride as _,
    };
    let samples = image::flat::FlatSamples {
        samples: data,
        layout,
        color_hint: None,
    };
    samples
        .try_into_buffer::<image::Bgra<u8>>()
        .or_else::<&str, _>(|(_err, samples)| {
            let view = samples.as_view::<image::Bgra<u8>>().unwrap();
            let mut img = BgraImage::new(width, height);
            img.copy_from(&view, 0, 0).unwrap();
            Ok(img)
        })
        .unwrap()
}

fn main() -> Result<(), Box<dyn Error>> {
    let args = Cli::parse();

    let listener = TcpListener::bind::<std::net::SocketAddr>(args.address.into()).unwrap();
    let dbus = if let Some(addr) = args.dbus_address {
        Connection::new_for_address(&addr, true)
    } else {
        Connection::new_session()
    }
    .expect("Failed to connect to DBus");

    let vm_name = VMProxy::new(&dbus)?.name()?;
    let console = Console::new(&dbus, 0).expect("Failed to get the console");
    let server = Server::new(vm_name, console)?;
    for stream in listener.incoming() {
        server.handle_client(stream?)?;
    }

    Ok(())
}
