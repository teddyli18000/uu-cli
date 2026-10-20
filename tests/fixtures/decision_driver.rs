#[cfg(windows)]
mod windows {
    use std::ffi::c_void;
    use std::fs::{File, OpenOptions};
    use std::io;
    use std::os::windows::io::IntoRawHandle;
    use std::path::Path;
    use std::thread;
    use std::time::{Duration, Instant};

    type Handle = *mut c_void;
    const TH32CS_SNAPPROCESS: u32 = 0x0000_0002;
    const INVALID_HANDLE_VALUE: Handle = -1isize as Handle;

    #[repr(C)]
    struct ProcessEntry32W {
        size: u32, usage: u32, process_id: u32, default_heap_id: usize,
        module_id: u32, threads: u32, parent_process_id: u32, priority: i32,
        flags: u32, executable: [u16; 260],
    }
    #[repr(C)]
    #[derive(Clone, Copy)]
    struct Coord { x: i16, y: i16 }
    #[repr(C)]
    #[derive(Clone, Copy)]
    struct SmallRect { left: i16, top: i16, right: i16, bottom: i16 }
    #[repr(C)]
    struct ScreenInfo {
        size: Coord, cursor: Coord, attributes: u16, window: SmallRect,
        maximum_window_size: Coord,
    }
    #[repr(C)]
    struct KeyEvent {
        key_down: i32, repeat_count: u16, virtual_key: u16, virtual_scan: u16,
        unicode_char: u16, control_state: u32,
    }
    #[repr(C)]
    struct InputRecord { event_type: u16, key_event: KeyEvent }

    #[link(name = "kernel32")]
    extern "system" {
        fn AttachConsole(process_id: u32) -> i32;
        fn CreateToolhelp32Snapshot(flags: u32, process_id: u32) -> Handle;
        fn Process32FirstW(snapshot: Handle, entry: *mut ProcessEntry32W) -> i32;
        fn Process32NextW(snapshot: Handle, entry: *mut ProcessEntry32W) -> i32;
        fn CloseHandle(handle: Handle) -> i32;
        fn GetConsoleScreenBufferInfo(output: Handle, info: *mut ScreenInfo) -> i32;
        fn ReadConsoleOutputCharacterW(output: Handle, buffer: *mut u16, length: u32, start: Coord, read: *mut u32) -> i32;
        fn WriteConsoleInputW(input: Handle, records: *const InputRecord, length: u32, written: *mut u32) -> i32;
        fn GetConsoleWindow() -> Handle;
    }
    #[link(name = "user32")]
    extern "system" {
        fn PostMessageW(window: Handle, message: u32, wparam: usize, lparam: isize) -> i32;
    }

    struct Snapshot(Handle);
    impl Drop for Snapshot {
        fn drop(&mut self) { unsafe { CloseHandle(self.0); } }
    }

    fn find_child(parent: u32) -> io::Result<Option<u32>> {
        let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
        if snapshot == INVALID_HANDLE_VALUE { return Err(io::Error::last_os_error()); }
        let _snapshot = Snapshot(snapshot);
        let mut item = ProcessEntry32W {
            size: std::mem::size_of::<ProcessEntry32W>() as u32,
            usage: 0, process_id: 0, default_heap_id: 0, module_id: 0, threads: 0,
            parent_process_id: 0, priority: 0, flags: 0, executable: [0; 260],
        };
        let mut found = unsafe { Process32FirstW(snapshot, &mut item) } != 0;
        while found {
            let name = String::from_utf16_lossy(&item.executable);
            if item.parent_process_id == parent && name.trim_end_matches('\0').eq_ignore_ascii_case("uu.exe") {
                return Ok(Some(item.process_id));
            }
            found = unsafe { Process32NextW(snapshot, &mut item) } != 0;
        }
        Ok(None)
    }

    fn attach(parent: u32, deadline: Instant) -> io::Result<(File, File)> {
        loop {
            if let Some(child) = find_child(parent)? {
                if unsafe { AttachConsole(child) } != 0 {
                    return Ok((
                        OpenOptions::new().write(true).open("CONIN$")?,
                        OpenOptions::new().read(true).open("CONOUT$")?,
                    ));
                }
                let error = io::Error::last_os_error();
                if Instant::now() >= deadline { return Err(error); }
            } else if Instant::now() >= deadline {
                return Err(io::Error::new(io::ErrorKind::TimedOut, "uu.exe decision console did not appear"));
            }
            thread::sleep(Duration::from_millis(25));
        }
    }

    fn screen(output: Handle) -> io::Result<String> {
        let mut info = ScreenInfo {
            size: Coord { x: 0, y: 0 }, cursor: Coord { x: 0, y: 0 }, attributes: 0,
            window: SmallRect { left: 0, top: 0, right: 0, bottom: 0 },
            maximum_window_size: Coord { x: 0, y: 0 },
        };
        if unsafe { GetConsoleScreenBufferInfo(output, &mut info) } == 0 { return Err(io::Error::last_os_error()); }
        let width = usize::try_from(info.size.x).unwrap_or_default();
        let count = width.saturating_mul(usize::try_from(info.size.y).unwrap_or_default());
        if width == 0 || count == 0 || count > 500_000 { return Err(io::Error::new(io::ErrorKind::InvalidData, "invalid console buffer size")); }
        let mut chars = vec![0u16; count];
        let mut read = 0;
        if unsafe { ReadConsoleOutputCharacterW(output, chars.as_mut_ptr(), count as u32, Coord { x: 0, y: 0 }, &mut read) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(chars[..read as usize].chunks(width).map(|row| String::from_utf16_lossy(row).trim_end().to_owned()).collect::<Vec<_>>().join("\n"))
    }

    fn record(down: bool, vk: u16, ch: u16) -> InputRecord {
        InputRecord { event_type: 1, key_event: KeyEvent {
            key_down: if down { 1 } else { 0 }, repeat_count: 1, virtual_key: vk,
            virtual_scan: 0, unicode_char: ch, control_state: 0,
        } }
    }
    fn write_keys(input: Handle, chars: impl IntoIterator<Item = (u16, u16)>) -> io::Result<()> {
        let mut events = Vec::new();
        for (vk, ch) in chars { events.push(record(true, vk, ch)); events.push(record(false, vk, ch)); }
        let mut written = 0;
        if unsafe { WriteConsoleInputW(input, events.as_ptr(), events.len() as u32, &mut written) } == 0 {
            return Err(io::Error::last_os_error());
        }
        if written != events.len() as u32 { return Err(io::Error::new(io::ErrorKind::WriteZero, "console input was incomplete")); }
        Ok(())
    }
    fn action(input: Handle, value: &str) -> io::Result<()> {
        match value {
            "wait" => Ok(()),
            "enter" => write_keys(input, [(0x0d, 13)]),
            "n" => write_keys(input, [(0x4e, b'n' as u16)]),
            "escape" => write_keys(input, [(0x1b, 27)]),
            "close" => {
                let window = unsafe { GetConsoleWindow() };
                if window.is_null() || unsafe { PostMessageW(window, 0x0010, 0, 0) } == 0 { Err(io::Error::last_os_error()) } else { Ok(()) }
            }
            value if value.starts_with("line:") => {
                let mut keys: Vec<_> = value[5..].encode_utf16().map(|ch| (0, ch)).collect();
                keys.push((0x0d, 13)); write_keys(input, keys)
            }
            _ => Err(io::Error::new(io::ErrorKind::InvalidInput, "invalid action")),
        }
    }
    pub fn run() -> Result<(), Box<dyn std::error::Error>> {
        let args: Vec<String> = std::env::args().skip(1).collect();
        if args.len() != 5 { return Err("usage: decision-driver <parent-pid> <timeout-ms> <transcript> <needle> <action>".into()); }
        let parent: u32 = args[0].parse()?;
        let deadline = Instant::now() + Duration::from_millis(args[1].parse()?);
        let (input, output) = attach(parent, deadline)?;
        let input = input.into_raw_handle() as Handle;
        let output = output.into_raw_handle() as Handle;
        loop {
            let captured = screen(output)?;
            std::fs::write(Path::new(&args[2]), &captured)?;
            if captured.contains(&args[3]) {
                action(input, &args[4])?;
                thread::sleep(Duration::from_millis(100));
                std::fs::write(Path::new(&args[2]), screen(output)?)?;
                unsafe { CloseHandle(input); CloseHandle(output); }
                return Ok(());
            }
            if Instant::now() >= deadline { return Err(format!("console did not show {:?} before timeout; transcript:\n{captured}", args[3]).into()); }
            thread::sleep(Duration::from_millis(25));
        }
    }
}
#[cfg(windows)]
fn main() {
    if let Err(error) = windows::run() { eprintln!("decision-driver: {error}"); std::process::exit(1); }
}
#[cfg(not(windows))]
fn main() {}
