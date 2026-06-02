// SPDX-License-Identifier: GPL-3.0-only
//! Raise (activate) the window whose title contains the given substring.
//!
//!   cargo run --release -p cosmic-applet-claude-sessions --example jump -- "<title substring>"
//!
//! Case-insensitive. With no argument, lists window titles instead.

use cosmic_client_toolkit::{
    toplevel_info::{ToplevelInfoHandler, ToplevelInfoState},
    toplevel_management::{ToplevelManagerHandler, ToplevelManagerState},
    cosmic_protocols::toplevel_management::v1::client::zcosmic_toplevel_manager_v1,
    wayland_client::{
        globals::registry_queue_init,
        protocol::{wl_output, wl_seat},
        Connection, QueueHandle, WEnum,
    },
    wayland_protocols::ext::foreign_toplevel_list::v1::client::ext_foreign_toplevel_handle_v1,
};
use sctk::{
    output::{OutputHandler, OutputState},
    registry::{ProvidesRegistryState, RegistryState},
    seat::{Capability, SeatHandler, SeatState},
};

struct AppData {
    registry_state: RegistryState,
    output_state: OutputState,
    seat_state: SeatState,
    toplevel_info: ToplevelInfoState,
    toplevel_manager: ToplevelManagerState,
}

fn main() {
    let query = std::env::args().nth(1);

    let conn = Connection::connect_to_env().expect("connect wayland");
    let (globals, mut event_queue) = registry_queue_init::<AppData>(&conn).unwrap();
    let qh = event_queue.handle();
    let registry_state = RegistryState::new(&globals);
    let mut app = AppData {
        output_state: OutputState::new(&globals, &qh),
        seat_state: SeatState::new(&globals, &qh),
        toplevel_info: ToplevelInfoState::new(&registry_state, &qh),
        toplevel_manager: ToplevelManagerState::new(&registry_state, &qh),
        registry_state,
    };

    for _ in 0..20 {
        event_queue.roundtrip(&mut app).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(50));
    }

    let Some(query) = query else {
        eprintln!("usage: jump \"<title substring>\"\n\nopen window titles:");
        for info in app.toplevel_info.toplevels() {
            eprintln!("  [{}] {:?}", info.app_id, info.title);
        }
        return;
    };

    let q = query.to_lowercase();
    let target = app
        .toplevel_info
        .toplevels()
        .find(|info| info.title.to_lowercase().contains(&q))
        .and_then(|info| info.cosmic_toplevel.clone());

    match (target, app.seat_state.seats().next()) {
        (Some(handle), Some(seat)) => {
            app.toplevel_manager.manager.activate(&handle, &seat);
            conn.flush().unwrap();
            // Roundtrip so the activate request is delivered before we exit.
            event_queue.roundtrip(&mut app).unwrap();
            println!("activated a window matching {query:?}");
        }
        (None, _) => eprintln!("no window title contains {query:?}"),
        (_, None) => eprintln!("no wl_seat available"),
    }
}

impl ProvidesRegistryState for AppData {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry_state
    }
    sctk::registry_handlers!(OutputState, SeatState);
}

impl OutputHandler for AppData {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.output_state
    }
    fn new_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn update_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn output_destroyed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
}

impl SeatHandler for AppData {
    fn seat_state(&mut self) -> &mut SeatState {
        &mut self.seat_state
    }
    fn new_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}
    fn new_capability(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: wl_seat::WlSeat,
        _: Capability,
    ) {
    }
    fn remove_capability(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: wl_seat::WlSeat,
        _: Capability,
    ) {
    }
    fn remove_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}
}

impl ToplevelInfoHandler for AppData {
    fn toplevel_info_state(&mut self) -> &mut ToplevelInfoState {
        &mut self.toplevel_info
    }
    fn new_toplevel(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &ext_foreign_toplevel_handle_v1::ExtForeignToplevelHandleV1,
    ) {
    }
    fn update_toplevel(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &ext_foreign_toplevel_handle_v1::ExtForeignToplevelHandleV1,
    ) {
    }
    fn toplevel_closed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &ext_foreign_toplevel_handle_v1::ExtForeignToplevelHandleV1,
    ) {
    }
}

impl ToplevelManagerHandler for AppData {
    fn toplevel_manager_state(&mut self) -> &mut ToplevelManagerState {
        &mut self.toplevel_manager
    }
    fn capabilities(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: Vec<WEnum<zcosmic_toplevel_manager_v1::ZcosmicToplelevelManagementCapabilitiesV1>>,
    ) {
    }
}

sctk::delegate_registry!(AppData);
sctk::delegate_output!(AppData);
sctk::delegate_seat!(AppData);
cosmic_client_toolkit::delegate_toplevel_info!(AppData);
cosmic_client_toolkit::delegate_toplevel_manager!(AppData);
