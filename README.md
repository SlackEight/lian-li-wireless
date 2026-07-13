# lian-li-wireless

Linux support for Lian Li's 2.4GHz wireless ecosystem — UNI FAN wireless
(SL-INF, SL V3, TL, CL), Strimer Wireless, and wireless AIOs — with a focus
on rock-solid fan control and full dynamic RGB.

**Status: M1 — protocol library + proof CLI.** Not yet a daemon; see
`docs/superpowers/specs/` for the design and roadmap (reliability daemon,
effect engine, Tauri UI).

## Requirements

- A Lian Li wireless TX/RX dongle pair (V1 `0416:8040/8041` or V2
  `1A86:E304/E305`) with devices already bound (bind via L-Connect or
  lian-li-linux for now; native binding lands with the UI milestone).
- udev permissions on the dongles (the lian-li-linux package's rules work;
  standalone rules ship with the packaging milestone).
- Stop any other software that owns the dongles (lianli-daemon, etc.) —
  only one process can drive them.

## Build & try

    git clone --recurse-submodules <repo-url>
    cargo build --release
    ./target/release/llw scan       # find the master dongle's RF channel
    ./target/release/llw devices    # list bound wireless devices
    ./target/release/llw set-pwm 0 60 --hold
    ./target/release/llw set-color 1 FF0000

## License

MIT. Protocol knowledge ported from
[sgtaziz/lian-li-linux](https://github.com/sgtaziz/lian-li-linux) (MIT) —
see NOTICE.
