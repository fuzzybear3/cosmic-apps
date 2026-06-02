// SPDX-License-Identifier: GPL-3.0-only
//! Diagnostic: list every toplevel window with its app_id and title, in the
//! order cctk yields them (which is the order the focus fallback would pick).

use cosmic_client_toolkit::{
    toplevel_info::{ToplevelInfoHandler, ToplevelInfoState},
    wayland_client::{globals::registry_queue_init, protocol::wl_output, Connection, QueueHandle},
    wayland_protocols::ext::foreign_toplevel_list::v1::client::ext_foreign_toplevel_handle_v1,
};
use sctk::{
    output::{OutputHandler, OutputState},
    registry::{ProvidesRegistryState, RegistryState},
};

struct AppData {
    registry_state: RegistryState,
    output_state: OutputState,
    toplevel_info: ToplevelInfoState,
}

fn main() {
    let conn = Connection::connect_to_env().expect("connect wayland");
    let (globals, mut event_queue) = registry_queue_init::<AppData>(&conn).unwrap();
    let qh = event_queue.handle();
    let registry_state = RegistryState::new(&globals);
    let mut app = AppData {
        output_state: OutputState::new(&globals, &qh),
        toplevel_info: ToplevelInfoState::new(&registry_state, &qh),
        registry_state,
    };

    // Roundtrip a few times with small pauses so the compositor has time to
    // send the initial toplevel list plus their cosmic state.
    for _ in 0..20 {
        event_queue.roundtrip(&mut app).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(50));
    }

    let mut count = 0;
    println!("{:<28} | {:<22} | title", "app_id", "state");
    println!("{:-<28}-+-{:-<22}-+-{:-<30}", "", "", "");
    for info in app.toplevel_info.toplevels() {
        count += 1;
        // `state` is a set of zcosmic states: Activated (focused), Maximized,
        // Minimized, Fullscreen, Sticky. Empty when the compositor only gave
        // the ext-foreign-toplevel info (no cosmic extension).
        let states: Vec<String> = info.state.iter().map(|s| format!("{s:?}")).collect();
        println!(
            "{:<28} | {:<22} | {:?}",
            info.app_id,
            states.join(","),
            info.title
        );
    }
    eprintln!("\n{count} toplevel(s). 'Activated' in the state column = focused window.");
}

impl ProvidesRegistryState for AppData {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry_state
    }
    sctk::registry_handlers!(OutputState);
}

impl OutputHandler for AppData {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.output_state
    }
    fn new_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn update_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn output_destroyed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
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

sctk::delegate_registry!(AppData);
sctk::delegate_output!(AppData);
cosmic_client_toolkit::delegate_toplevel_info!(AppData);
