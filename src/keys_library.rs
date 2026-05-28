//! Library of named Windows Virtual-Key codes, matching the reference
//! ControlPad SelectKeyPopup. Used by the wizard step-1 picker so users can
//! pick keys by name instead of typing decimal VK codes.

#[derive(Clone, Copy)]
pub struct NamedKey {
    pub name: &'static str,
    pub vk: u32,
}

pub const KEYS: &[NamedKey] = &[
    // Letters
    NamedKey { name: "A", vk: 0x41 }, NamedKey { name: "B", vk: 0x42 },
    NamedKey { name: "C", vk: 0x43 }, NamedKey { name: "D", vk: 0x44 },
    NamedKey { name: "E", vk: 0x45 }, NamedKey { name: "F", vk: 0x46 },
    NamedKey { name: "G", vk: 0x47 }, NamedKey { name: "H", vk: 0x48 },
    NamedKey { name: "I", vk: 0x49 }, NamedKey { name: "J", vk: 0x4A },
    NamedKey { name: "K", vk: 0x4B }, NamedKey { name: "L", vk: 0x4C },
    NamedKey { name: "M", vk: 0x4D }, NamedKey { name: "N", vk: 0x4E },
    NamedKey { name: "O", vk: 0x4F }, NamedKey { name: "P", vk: 0x50 },
    NamedKey { name: "Q", vk: 0x51 }, NamedKey { name: "R", vk: 0x52 },
    NamedKey { name: "S", vk: 0x53 }, NamedKey { name: "T", vk: 0x54 },
    NamedKey { name: "U", vk: 0x55 }, NamedKey { name: "V", vk: 0x56 },
    NamedKey { name: "W", vk: 0x57 }, NamedKey { name: "X", vk: 0x58 },
    NamedKey { name: "Y", vk: 0x59 }, NamedKey { name: "Z", vk: 0x5A },

    // Numbers (top row)
    NamedKey { name: "0", vk: 0x30 }, NamedKey { name: "1", vk: 0x31 },
    NamedKey { name: "2", vk: 0x32 }, NamedKey { name: "3", vk: 0x33 },
    NamedKey { name: "4", vk: 0x34 }, NamedKey { name: "5", vk: 0x35 },
    NamedKey { name: "6", vk: 0x36 }, NamedKey { name: "7", vk: 0x37 },
    NamedKey { name: "8", vk: 0x38 }, NamedKey { name: "9", vk: 0x39 },

    // Function keys
    NamedKey { name: "F1", vk: 0x70 },  NamedKey { name: "F2", vk: 0x71 },
    NamedKey { name: "F3", vk: 0x72 },  NamedKey { name: "F4", vk: 0x73 },
    NamedKey { name: "F5", vk: 0x74 },  NamedKey { name: "F6", vk: 0x75 },
    NamedKey { name: "F7", vk: 0x76 },  NamedKey { name: "F8", vk: 0x77 },
    NamedKey { name: "F9", vk: 0x78 },  NamedKey { name: "F10", vk: 0x79 },
    NamedKey { name: "F11", vk: 0x7A }, NamedKey { name: "F12", vk: 0x7B },
    NamedKey { name: "F13", vk: 0x7C }, NamedKey { name: "F14", vk: 0x7D },
    NamedKey { name: "F15", vk: 0x7E }, NamedKey { name: "F16", vk: 0x7F },
    NamedKey { name: "F17", vk: 0x80 }, NamedKey { name: "F18", vk: 0x81 },
    NamedKey { name: "F19", vk: 0x82 }, NamedKey { name: "F20", vk: 0x83 },
    NamedKey { name: "F21", vk: 0x84 }, NamedKey { name: "F22", vk: 0x85 },
    NamedKey { name: "F23", vk: 0x86 }, NamedKey { name: "F24", vk: 0x87 },

    // Control keys
    NamedKey { name: "Backspace", vk: 0x08 },
    NamedKey { name: "Tab", vk: 0x09 },
    NamedKey { name: "Enter", vk: 0x0D },
    NamedKey { name: "Shift", vk: 0x10 },
    NamedKey { name: "Ctrl", vk: 0x11 },
    NamedKey { name: "Alt", vk: 0x12 },
    NamedKey { name: "Pause", vk: 0x13 },
    NamedKey { name: "Caps Lock", vk: 0x14 },
    NamedKey { name: "Escape", vk: 0x1B },
    NamedKey { name: "Space", vk: 0x20 },
    NamedKey { name: "Page Up", vk: 0x21 },
    NamedKey { name: "Page Down", vk: 0x22 },
    NamedKey { name: "End", vk: 0x23 },
    NamedKey { name: "Home", vk: 0x24 },
    NamedKey { name: "Left Arrow", vk: 0x25 },
    NamedKey { name: "Up Arrow", vk: 0x26 },
    NamedKey { name: "Right Arrow", vk: 0x27 },
    NamedKey { name: "Down Arrow", vk: 0x28 },
    NamedKey { name: "Insert", vk: 0x2D },
    NamedKey { name: "Delete", vk: 0x2E },
    NamedKey { name: "Windows (Left)", vk: 0x5B },
    NamedKey { name: "Windows (Right)", vk: 0x5C },
    NamedKey { name: "Menu", vk: 0x5D },
    NamedKey { name: "Print Screen", vk: 0x2C },
    NamedKey { name: "Scroll Lock", vk: 0x91 },
    NamedKey { name: "Num Lock", vk: 0x90 },
    NamedKey { name: "Left Shift", vk: 0xA0 },
    NamedKey { name: "Right Shift", vk: 0xA1 },
    NamedKey { name: "Left Ctrl", vk: 0xA2 },
    NamedKey { name: "Right Ctrl", vk: 0xA3 },
    NamedKey { name: "Left Alt", vk: 0xA4 },
    NamedKey { name: "Right Alt", vk: 0xA5 },

    // Numpad
    NamedKey { name: "Numpad 0", vk: 0x60 },
    NamedKey { name: "Numpad 1", vk: 0x61 },
    NamedKey { name: "Numpad 2", vk: 0x62 },
    NamedKey { name: "Numpad 3", vk: 0x63 },
    NamedKey { name: "Numpad 4", vk: 0x64 },
    NamedKey { name: "Numpad 5", vk: 0x65 },
    NamedKey { name: "Numpad 6", vk: 0x66 },
    NamedKey { name: "Numpad 7", vk: 0x67 },
    NamedKey { name: "Numpad 8", vk: 0x68 },
    NamedKey { name: "Numpad 9", vk: 0x69 },
    NamedKey { name: "Numpad *", vk: 0x6A },
    NamedKey { name: "Numpad +", vk: 0x6B },
    NamedKey { name: "Numpad -", vk: 0x6D },
    NamedKey { name: "Numpad .", vk: 0x6E },
    NamedKey { name: "Numpad /", vk: 0x6F },
    NamedKey { name: "Numpad Enter", vk: 0x0D },

    // Media keys
    NamedKey { name: "Volume Mute", vk: 0xAD },
    NamedKey { name: "Volume Down", vk: 0xAE },
    NamedKey { name: "Volume Up", vk: 0xAF },
    NamedKey { name: "Media Next", vk: 0xB0 },
    NamedKey { name: "Media Previous", vk: 0xB1 },
    NamedKey { name: "Media Stop", vk: 0xB2 },
    NamedKey { name: "Media Play/Pause", vk: 0xB3 },

    // Browser
    NamedKey { name: "Browser Back", vk: 0xA6 },
    NamedKey { name: "Browser Forward", vk: 0xA7 },
    NamedKey { name: "Browser Refresh", vk: 0xA8 },
    NamedKey { name: "Browser Stop", vk: 0xA9 },
    NamedKey { name: "Browser Search", vk: 0xAA },
    NamedKey { name: "Browser Home", vk: 0xAC },

    // Other launch keys
    NamedKey { name: "Launch Mail", vk: 0xB4 },
    NamedKey { name: "Launch Media", vk: 0xB5 },

    // Punctuation
    NamedKey { name: ";", vk: 0xBA },
    NamedKey { name: "=", vk: 0xBB },
    NamedKey { name: ",", vk: 0xBC },
    NamedKey { name: "-", vk: 0xBD },
    NamedKey { name: ".", vk: 0xBE },
    NamedKey { name: "/", vk: 0xBF },
    NamedKey { name: "`", vk: 0xC0 },
    NamedKey { name: "[", vk: 0xDB },
    NamedKey { name: "\\", vk: 0xDC },
    NamedKey { name: "]", vk: 0xDD },
    NamedKey { name: "'", vk: 0xDE },
];

pub fn label_for_vk(vk: u32) -> Option<&'static str> {
    KEYS.iter().find(|k| k.vk == vk).map(|k| k.name)
}
