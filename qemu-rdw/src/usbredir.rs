use glib::{clone, MainContext};
use gtk::{glib, prelude::*};
use qemu_display::UsbRedir;
use rdw::gtk;

#[derive(Clone, Debug)]
pub struct Handler {
    usbredir: UsbRedir,
}

impl Handler {
    pub fn new(usbredir: UsbRedir) -> Self {
        Self { usbredir }
    }

    pub fn widget(&self) -> rdw::UsbRedir {
        let widget = rdw::UsbRedir::new();

        let usbredir = self.usbredir.clone();
        widget
            .model()
            .connect_items_changed(clone!(@weak widget => move |model, pos, _rm, add| {
                let usbredir = usbredir.clone();
                MainContext::default().spawn_local(clone!(@weak model => async move {
                    for pos in pos..pos + add {
                        let item = model.item(pos).unwrap();
                        if let Some(dev) = item.downcast_ref::<rdw::UsbDevice>().unwrap().device() {
                            item.set_property("active", usbredir.is_device_connected(&dev).await);
                        }
                    }
                }));
            }));

        let usbredir = self.usbredir.clone();
        widget.connect_device_state_set(move |widget, item, state| {
            let device = match item.device() {
                Some(it) => it,
                _ => return,
            };

            let usbredir = usbredir.clone();
            MainContext::default().spawn_local(clone!(@weak item, @weak widget => async move {
                match usbredir.set_device_state(&device, state).await {
                    Ok(active) => item.set_property("active", active),
                    Err(e) => {
                        if state {
                            item.set_property("active", false);
                        }
                        widget.emit_by_name::<bool>("show-error", &[&e.to_string()]);
                    },
                }
            }));
        });

        let usbredir = self.usbredir.clone();
        MainContext::default().spawn_local(clone!(@weak widget => async move {
            use futures::stream::StreamExt; // for `next`
            widget
                .set_property("free-channels", usbredir.n_free_channels().await);
            let mut n = usbredir.receive_n_free_channels().await;
            while let Some(n) = n.next().await {
                widget.set_property("free-channels", n);
            }
        }));

        widget
    }
}
