# lian-li-wireless

Linux support for Lian Li's 2.4GHz wireless ecosystem — UNI FAN wireless
(SL-INF, SL V3, TL, CL), Strimer Wireless, and wireless AIOs — with a focus
on rock-solid fan control and full dynamic RGB.

**Status: M2 — reliability daemon (fans + RGB + link supervision) with systemd/udev packaging.** See
`docs/superpowers/specs/` for the design and roadmap (effect engine, Tauri UI).

## Requirements

- A Lian Li wireless TX/RX dongle pair (V1 `0416:8040/8041` or V2
  `1A86:E304/E305`) with devices already bound (bind via L-Connect or
  lian-li-linux for now; native binding lands with the UI milestone).

## Build

    git clone --recurse-submodules <repo-url>
    cargo build --release

## Install (daemon)

The `llw-daemon` replaces `lianli-daemon` and both **cannot run simultaneously** — only one process can own the dongles. If you're running `lianli-daemon`, it must be disabled before starting `llw-daemon`.

### Installation steps

1. **Build a release binary:**
   ```bash
   cargo build --release
   ```

2. **Install the daemon binary:**
   ```bash
   sudo install -Dm755 target/release/llw-daemon /usr/local/bin/llw-daemon
   ```

3. **Install udev rules and reload:**
   ```bash
   sudo install -Dm644 packaging/udev/99-llw.rules /etc/udev/rules.d/99-llw.rules
   sudo udevadm control --reload
   ```

4. **Install systemd user unit:**
   ```bash
   install -Dm644 packaging/systemd/llw-daemon.service ~/.config/systemd/user/llw-daemon.service
   ```

5. **Import configuration from lianli-daemon (if present):**
   ```bash
   cargo run -p llw-daemon -- --import-lianli
   ```
   Review any warnings and verify with:
   ```bash
   cargo run -p llw-daemon -- --check-config
   ```

6. **Switch daemons (disable lianli, enable llw):**
   ```bash
   systemctl --user disable --now lianli-watchdog lianli-daemon
   systemctl --user enable --now llw-daemon
   ```

7. **Verify operation:**
   ```bash
   llw status
   ```
   Expected: link acquired on the device-reported channel, fans at curve PWM, RGB asserted, no TX wedge flag.

## Try the CLI

    ./target/release/llw scan       # find the master dongle's RF channel
    ./target/release/llw devices    # list bound wireless devices
    ./target/release/llw set-pwm 0 60 --hold
    ./target/release/llw set-color 1 FF0000
    ./target/release/llw status     # show daemon status (requires daemon running)

## Desktop app (llw-ui)

Tauri 2 + React desktop app over the daemon: Health (link/reliability/sync),
Devices (bind/unbind, rename), Lighting and Cooling (in progress).

Development (requires `npm` and the Tauri system deps — `webkit2gtk-4.1` on Arch):

    cd crates/llw-ui/ui && npm install
    cd .. && cargo tauri dev

The daemon must be running (the app shows an amber "daemon unreachable" banner
otherwise and reconnects automatically). Production build: `cargo tauri build`
(packaging lands in M5).

## License

MIT. Protocol knowledge ported from
[sgtaziz/lian-li-linux](https://github.com/sgtaziz/lian-li-linux) (MIT) —
see NOTICE.
