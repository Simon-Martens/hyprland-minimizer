//! Main application logic for the minimize-to-tray utility.
//! Place this file in the `src/` directory of your Rust project.

use anyhow::{anyhow, Context, Result};
use clap::Parser;
use serde::Deserialize;
use std::collections::HashMap;
use std::process::Command;
use std::sync::Arc;
use tokio::sync::Notify;
use tokio::time::{interval, Duration};
use zbus::zvariant::{ObjectPath, Value};
use zbus::{dbus_interface, ConnectionBuilder, Proxy};

// --- Command-Line Interface Definition ---
#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// The address of the window to minimize. If not provided, minimizes the active window.
    window_address: Option<String>,
}

// --- Hyprland Data Structures ---
// These structs are used to deserialize the JSON output from `hyprctl`.

#[derive(Deserialize, Debug, Clone)]
struct Workspace {
    id: i32,
}

#[derive(Deserialize, Debug, Clone)]
#[allow(dead_code)]
struct WindowInfo {
    address: String,
    workspace: Workspace,
    title: String,
    class: String,
}

// --- Hyprland Interaction Functions ---

/// Executes a hyprctl command and returns the parsed JSON output.
fn hyprctl<T: for<'de> Deserialize<'de>>(command: &str) -> Result<T> {
    let output = Command::new("hyprctl")
        .arg("-j")
        .arg(command)
        .output()
        .with_context(|| format!("Failed to execute hyprctl command: {}", command))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("hyprctl command '{}' failed: {}", command, stderr);
    }

    serde_json::from_slice(&output.stdout)
        .with_context(|| format!("Failed to parse JSON from hyprctl command: {}", command))
}

/// Executes a hyprctl dispatch command.
fn hyprctl_dispatch(command: &str) -> Result<()> {
    let status = Command::new("hyprctl")
        .arg("dispatch")
        .arg(command)
        .status()
        .with_context(|| format!("Failed to execute hyprctl dispatch: {}", command))?;

    if !status.success() {
        anyhow::bail!("hyprctl dispatch command '{}' failed", command);
    }
    Ok(())
}

/// Finds a window by its address from the list of all clients.
fn get_window_by_address(address: &str) -> Result<WindowInfo> {
    let clients: Vec<WindowInfo> =
        hyprctl("clients").context("Failed to get client list from Hyprland.")?;
    clients
        .into_iter()
        .find(|c| c.address == address)
        .ok_or_else(|| anyhow!("Could not find a window with address '{}'", address))
}

// --- D-Bus Menu Implementation ---

struct DbusMenu {
    window_info: WindowInfo,
    exit_notify: Arc<Notify>,
}

#[dbus_interface(name = "com.canonical.dbusmenu")]
impl DbusMenu {
    /// Returns the menu layout.
    fn get_layout(
        &self,
        _parent_id: i32,
        _recursion_depth: i32,
        _property_names: Vec<String>,
    ) -> (u32, (i32, HashMap<String, Value>, Vec<Value>)) {
        println!("[D-Bus Menu] GetLayout called.");

        // Item ID 1: Open on current workspace
        let mut open_props = HashMap::new();
        open_props.insert("type".to_string(), Value::from("standard"));
        open_props.insert(
            "label".to_string(),
            Value::from(format!("Open {}", self.window_info.title)),
        );
        let open_item = Value::from((1i32, open_props, Vec::<Value>::new()));

        // Item ID 2: Open on original workspace
        let mut last_ws_props = HashMap::new();
        last_ws_props.insert("type".to_string(), Value::from("standard"));
        last_ws_props.insert(
            "label".to_string(),
            Value::from(format!(
                "Open on original workspace ({})",
                self.window_info.workspace.id
            )),
        );
        let last_ws_item = Value::from((2i32, last_ws_props, Vec::<Value>::new()));

        // Item ID 3: Close the window
        let mut close_props = HashMap::new();
        close_props.insert("type".to_string(), Value::from("standard"));
        close_props.insert(
            "label".to_string(),
            Value::from(format!("Close {}", self.window_info.title)),
        );
        let close_item = Value::from((3i32, close_props, Vec::<Value>::new()));

        // The root of the menu layout
        let mut root_props = HashMap::new();
        root_props.insert("children-display".to_string(), Value::from("submenu"));

        let root_layout = (
            0i32, // Root node ID is always 0
            root_props,
            vec![open_item, last_ws_item, close_item],
        );

        // Incrementing the revision number helps ensure clients fetch the new layout
        let revision = 2u32;
        println!(
            "[D-Bus Menu] Serving layout revision {}: {:?}",
            revision, root_layout
        );
        (revision, root_layout)
    }

    /// Returns the properties for a group of menu items.
    fn get_group_properties(
        &self,
        ids: Vec<i32>,
        _property_names: Vec<String>,
    ) -> Vec<(i32, HashMap<String, Value>)> {
        println!("[D-Bus Menu] GetGroupProperties called for IDs: {:?}", ids);
        let mut result = Vec::new();
        for id in ids {
            let mut props = HashMap::new();
            let label = match id {
                1 => format!("Open {}", self.window_info.title),
                2 => format!(
                    "Open on original workspace ({})",
                    self.window_info.workspace.id
                ),
                3 => format!("Close {}", self.window_info.title),
                _ => continue,
            };
            props.insert("label".to_string(), Value::from(label));
            props.insert("enabled".to_string(), Value::from(true));
            props.insert("visible".to_string(), Value::from(true));
            props.insert("type".to_string(), Value::from("standard"));
            result.push((id, props));
        }
        println!("[D-Bus Menu] Returning properties: {:?}", result);
        result
    }

    /// Handles a batch of click events. This is called by Waybar instead of the singular `Event`.
    fn event_group(&self, events: Vec<(i32, String, Value<'_>, u32)>) {
        println!(
            "[D-Bus Menu] EventGroup received with {} events",
            events.len()
        );
        for (id, event_id, data, timestamp) in events {
            self.event(id, &event_id, data, timestamp);
        }
    }

    /// Handles a single click event on a menu item.
    fn event(&self, id: i32, event_id: &str, _data: Value<'_>, _timestamp: u32) {
        println!(
            "[D-Bus Menu] Event received: id='{}', event_id='{}'",
            id, event_id
        );
        if event_id == "clicked" {
            let res = match id {
                1 => {
                    // Open on current workspace
                    println!("[D-Bus Menu] 'Open' action triggered.");
                    match hyprctl::<Workspace>("activeworkspace") {
                        Ok(active_workspace) => hyprctl_dispatch(&format!(
                            "movetoworkspace {},address:{}",
                            active_workspace.id, self.window_info.address
                        ))
                        .and_then(|_| {
                            hyprctl_dispatch(&format!(
                                "focuswindow address:{}",
                                self.window_info.address
                            ))
                        }),
                        Err(e) => {
                            eprintln!("[Error] Failed to get active workspace: {}", e);
                            Err(e)
                        }
                    }
                }
                2 => {
                    // Open on original workspace
                    println!("[D-Bus Menu] 'Open on original workspace' action triggered.");
                    hyprctl_dispatch(&format!(
                        "movetoworkspace {},address:{}",
                        self.window_info.workspace.id, self.window_info.address
                    ))
                    .and_then(|_| {
                        hyprctl_dispatch(&format!(
                            "focuswindow address:{}",
                            self.window_info.address
                        ))
                    })
                }
                3 => {
                    // Close the window
                    println!("[D-Bus Menu] 'Close' action triggered.");
                    hyprctl_dispatch(&format!("closewindow address:{}", self.window_info.address))
                }
                _ => {
                    println!("[D-Bus Menu] Clicked on unknown item id: {}", id);
                    return;
                }
            };

            if let Err(e) = res {
                eprintln!(
                    "[Error] Failed to execute hyprctl dispatch from menu: {}",
                    e
                );
            }

            self.exit_notify.notify_one();
        }
    }

    /// Handles a batch of "about to show" requests.
    fn about_to_show_group(&self, ids: Vec<i32>) -> (Vec<i32>, Vec<i32>) {
        println!("[D-Bus Menu] AboutToShowGroup received for IDs: {:?}", ids);
        (vec![], vec![])
    }

    /// Kept for compatibility.
    fn about_to_show(&self, _id: i32) -> bool {
        false
    }

    #[dbus_interface(property)]
    fn version(&self) -> u32 {
        3
    }

    #[dbus_interface(property)]
    fn text_direction(&self) -> &str {
        "ltr"
    }

    #[dbus_interface(property)]
    fn status(&self) -> &str {
        "normal"
    }
}

// --- Status Notifier Item (Tray Icon) Implementation ---

struct StatusNotifierItem {
    window_info: WindowInfo,
    exit_notify: Arc<Notify>,
}

#[dbus_interface(name = "org.kde.StatusNotifierItem")]
impl StatusNotifierItem {
    // --- Properties ---
    #[dbus_interface(property)]
    fn category(&self) -> &str {
        "ApplicationStatus"
    }

    #[dbus_interface(property)]
    fn id(&self) -> &str {
        &self.window_info.class
    }

    #[dbus_interface(property)]
    fn title(&self) -> &str {
        &self.window_info.title
    }

    #[dbus_interface(property)]
    fn status(&self) -> &str {
        "Active"
    }

    #[dbus_interface(property)]
    fn icon_name(&self) -> &str {
        &self.window_info.class
    }

    #[dbus_interface(property)]
    fn tool_tip(&self) -> (String, Vec<(i32, i32, Vec<u8>)>, String, String) {
        (
            String::new(),
            Vec::new(),
            self.window_info.title.clone(),
            String::new(),
        )
    }

    #[dbus_interface(property)]
    fn item_is_menu(&self) -> bool {
        false
    }

    #[dbus_interface(property)]
    fn menu(&self) -> ObjectPath<'_> {
        ObjectPath::try_from("/Menu").unwrap()
    }

    // --- Methods ---
    fn activate(&self, _x: i32, _y: i32) {
        println!("[D-Bus] Activate called (left-click)");
        if let Ok(active_workspace) = hyprctl::<Workspace>("activeworkspace") {
            if let Err(e) = hyprctl_dispatch(&format!(
                "movetoworkspace {},address:{}",
                active_workspace.id, self.window_info.address
            ))
            .and_then(|_| {
                hyprctl_dispatch(&format!("focuswindow address:{}", self.window_info.address))
            }) {
                eprintln!("[Error] Failed to execute activate action: {}", e);
            }
        } else {
            eprintln!("[Error] Failed to get active workspace");
        }
        self.exit_notify.notify_one();
    }

    fn secondary_activate(&self, _x: i32, _y: i32) {
        println!("[D-Bus] SecondaryActivate called (middle-click to close)");
        if let Err(e) =
            hyprctl_dispatch(&format!("closewindow address:{}", self.window_info.address))
        {
            eprintln!("[Error] Failed to execute secondary_activate action: {}", e);
        }
        self.exit_notify.notify_one();
    }
}

// --- Main Application Logic ---

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();

    // 1. Get window info based on CLI arguments
    let mut window_info = if let Some(address) = args.window_address {
        println!("Attempting to minimize window with address: {}", address);
        get_window_by_address(&address)?
    } else {
        println!("No window address provided, minimizing active window.");
        hyprctl("activewindow").context("Failed to get active window. Is a window focused?")?
    };

    println!(
        "Minimizing window: '{}' ({}) from workspace {}",
        window_info.title, window_info.class, window_info.workspace.id
    );

    if window_info.class.is_empty() {
        // Fallback to title if class is empty, for better icon matching
        window_info.class = window_info.title.clone();
    }

    // 2. Move the window to the special "minimized" workspace
    hyprctl_dispatch(&format!(
        "movetoworkspacesilent special:minimized,address:{}",
        window_info.address
    ))?;

    // 3. Set up the D-Bus services
    let exit_notify = Arc::new(Notify::new());

    let notifier_item = StatusNotifierItem {
        window_info: window_info.clone(),
        exit_notify: Arc::clone(&exit_notify),
    };

    let dbus_menu = DbusMenu {
        window_info: window_info.clone(),
        exit_notify: Arc::clone(&exit_notify),
    };

    let bus_name = format!(
        "org.kde.StatusNotifierItem.minimizer.p{}",
        std::process::id()
    );

    let _connection = ConnectionBuilder::session()?
        .name(bus_name.as_str())?
        .serve_at("/StatusNotifierItem", notifier_item)?
        .serve_at("/Menu", dbus_menu)?
        .build()
        .await?;

    println!("D-Bus service '{}' is running.", bus_name);

    // 4. Register the item with the StatusNotifierWatcher
    let watcher_proxy: Proxy<'_> = zbus::ProxyBuilder::new_bare(&_connection)
        .interface("org.kde.StatusNotifierWatcher")?
        .path("/StatusNotifierWatcher")?
        .destination("org.kde.StatusNotifierWatcher")?
        .build()
        .await?;

    println!("Registering with StatusNotifierWatcher...");
    if let Err(e) = watcher_proxy
        .call_method("RegisterStatusNotifierItem", &(bus_name.as_str(),))
        .await
    {
        eprintln!("Could not register with StatusNotifierWatcher: {}", e);
        eprintln!("Is a tray like Waybar running?");
        let _ = hyprctl_dispatch(&format!(
            "movetoworkspace {},address:{}",
            window_info.workspace.id, window_info.address
        ));
        anyhow::bail!("Failed to register tray icon.");
    }
    println!("Registration successful.");

    // 5. Start a background check to see if the window is closed or moved
    let window_address = window_info.address.clone();
    let check_task_exit_notify = Arc::clone(&exit_notify);
    tokio::spawn(async move {
        let mut interval = interval(Duration::from_secs(2));
        loop {
            interval.tick().await;
            match hyprctl::<Vec<WindowInfo>>("clients") {
                Ok(clients) => {
                    if let Some(client) = clients.iter().find(|c| c.address == window_address) {
                        if client.workspace.id > 0 {
                            println!("Window restored externally. Exiting.");
                            check_task_exit_notify.notify_one();
                            break;
                        }
                    } else {
                        println!("Window closed externally. Exiting.");
                        check_task_exit_notify.notify_one();
                        break;
                    }
                }
                Err(e) => {
                    eprintln!("Error checking window state: {}", e);
                    check_task_exit_notify.notify_one();
                    break;
                }
            }
        }
    });

    // 6. Wait for a notification to exit
    println!("Application minimized to tray. Waiting for activation...");
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {
            println!("\nInterrupted by Ctrl+C. Restoring window.");
            let _ = hyprctl_dispatch(&format!(
                "movetoworkspace {},address:{}",
                window_info.workspace.id, window_info.address
            ));
        }
        _ = exit_notify.notified() => {
            println!("Exit notification received.");
        }
    }

    // 7. Cleanup is handled automatically when the connection is dropped.
    println!("Exiting.");
    Ok(())
}
