// Talks to Hyprland's own sockets and reads only the fields it needs, so new or renamed
// fields in newer Hyprland versions (0.57+ broke the `hyprland` crate's parser) don't matter.
use serde_json::Value;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::mpsc;
use std::thread;

#[derive(Clone)]
pub struct WindowRect {
    pub address: String,
    pub x: f32,
    pub y: f32,
    pub width: f32,
}

#[derive(Clone, Debug)]
pub struct MonitorRect {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    pub has_fullscreen: bool,
}

#[derive(Debug, Clone)]
pub enum HyprlandEvent {
    WindowsChanged,
}

fn socket(name: &str) -> Option<PathBuf> {
    let sig = std::env::var("HYPRLAND_INSTANCE_SIGNATURE").ok()?;
    let runtime = std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/tmp".into());
    let path = PathBuf::from(runtime).join("hypr").join(sig).join(name);
    path.exists().then_some(path)
}

fn query(what: &str) -> Option<Value> {
    let mut stream = UnixStream::connect(socket(".socket.sock")?).ok()?;
    stream.write_all(format!("j/{what}").as_bytes()).ok()?;
    let mut out = String::new();
    stream.read_to_string(&mut out).ok()?;
    serde_json::from_str(&out).ok()
}

fn num(v: &Value, key: &str) -> f32 {
    v.get(key).and_then(Value::as_f64).unwrap_or(0.0) as f32
}

// Monitor size in layout pixels: Hyprland reports physical width/height, scaled and possibly rotated.
fn logical_size(m: &Value) -> (f32, f32) {
    let scale = m.get("scale").and_then(Value::as_f64).unwrap_or(1.0).max(0.1) as f32;
    let (w, h) = (num(m, "width") / scale, num(m, "height") / scale);
    if m.get("transform").and_then(Value::as_i64).unwrap_or(0) % 2 == 1 { (h, w) } else { (w, h) }
}

pub fn get_total_screen_bounds() -> (f32, f32, f32, f32) {
    let monitors = query("monitors").and_then(|v| v.as_array().cloned()).unwrap_or_default();
    if monitors.is_empty() {
        return (0.0, 0.0, 1920.0, 1080.0);
    }
    let (mut min_x, mut min_y, mut max_x, mut max_y) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
    for m in &monitors {
        let (w, h) = logical_size(m);
        let (x, y) = (num(m, "x"), num(m, "y"));
        min_x = min_x.min(x);
        min_y = min_y.min(y);
        max_x = max_x.max(x + w);
        max_y = max_y.max(y + h);
    }
    (min_x, min_y, max_x, max_y)
}

pub fn get_hyprland_windows() -> Vec<WindowRect> {
    let Some(active) = query("activeworkspace").and_then(|w| w.get("id").and_then(Value::as_i64)) else {
        return Vec::new();
    };
    query("clients")
        .and_then(|v| v.as_array().cloned())
        .unwrap_or_default()
        .iter()
        .filter(|c| c.pointer("/workspace/id").and_then(Value::as_i64) == Some(active))
        .map(|c| WindowRect {
            address: c.get("address").and_then(Value::as_str).unwrap_or_default().to_string(),
            x: c.pointer("/at/0").and_then(Value::as_f64).unwrap_or(0.0) as f32,
            y: c.pointer("/at/1").and_then(Value::as_f64).unwrap_or(0.0) as f32,
            width: c.pointer("/size/0").and_then(Value::as_f64).unwrap_or(0.0) as f32,
        })
        .collect()
}

pub fn get_monitors_with_fullscreen_state() -> Vec<MonitorRect> {
    let workspaces = query("workspaces").and_then(|v| v.as_array().cloned()).unwrap_or_default();
    query("monitors")
        .and_then(|v| v.as_array().cloned())
        .unwrap_or_default()
        .iter()
        .map(|m| {
            let ws = m.pointer("/activeWorkspace/id").and_then(Value::as_i64);
            let has_fullscreen = workspaces.iter().any(|w| {
                w.get("id").and_then(Value::as_i64) == ws
                    && w.get("hasfullscreen").and_then(Value::as_bool).unwrap_or(false)
            });
            let (width, height) = logical_size(m);
            MonitorRect { x: num(m, "x"), y: num(m, "y"), width, height, has_fullscreen }
        })
        .collect()
}

pub fn spawn_event_listener() -> mpsc::Receiver<HyprlandEvent> {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let Some(stream) = socket(".socket2.sock").and_then(|p| UnixStream::connect(p).ok()) else {
            return;
        };
        for line in BufReader::new(stream).lines().map_while(Result::ok) {
            let event = line.split(">>").next().unwrap_or_default();
            if matches!(event, "openwindow" | "closewindow" | "movewindow" | "movewindowv2" | "activewindow"
                | "activewindowv2" | "workspace" | "workspacev2" | "fullscreen" | "changefloatingmode")
                && tx.send(HyprlandEvent::WindowsChanged).is_err()
            {
                break;
            }
        }
    });
    rx
}
