//! Windows Virtual-Key -> X11 keysym lookup.
//!
//! Split out of `keys.rs` and compiled on every platform (not just Linux) so
//! the table is unit-testable anywhere, including on the Windows CI runner.
//! `keys.rs` only consults it on non-Windows targets.

#![allow(dead_code)] // unused on Windows, where VK codes are passed through

/// Windows Virtual-Key → X11 keysym (`keysymdef.h` / `XF86keysym.h`).
pub fn vk_to_keysym(vk: u32) -> Option<u32> {
    // Letters map to the *lowercase* keysym: VK 0x41 ('A') on Windows produces
    // an unshifted keypress, and the X11 uppercase keysym would imply Shift.
    if (0x41..=0x5A).contains(&vk) {
        return Some(vk + 0x20);
    }
    // Digits and space share numbering with ASCII/keysyms.
    if (0x30..=0x39).contains(&vk) || vk == 0x20 {
        return Some(vk);
    }
    // F1..F24 -> XK_F1..XK_F24 (contiguous in both numberings).
    if (0x70..=0x87).contains(&vk) {
        return Some(0xFFBE + (vk - 0x70));
    }
    // Numpad 0..9 -> XK_KP_0..XK_KP_9.
    if (0x60..=0x69).contains(&vk) {
        return Some(0xFFB0 + (vk - 0x60));
    }
    Some(match vk {
        0x08 => 0xFF08, // Backspace
        0x09 => 0xFF09, // Tab
        0x0D => 0xFF0D, // Return
        0x10 => 0xFFE1, // Shift_L
        0x11 => 0xFFE3, // Control_L
        0x12 => 0xFFE9, // Alt_L
        0x13 => 0xFF13, // Pause
        0x14 => 0xFFE5, // Caps_Lock
        0x1B => 0xFF1B, // Escape
        0x21 => 0xFF55, // Prior (Page Up)
        0x22 => 0xFF56, // Next (Page Down)
        0x23 => 0xFF57, // End
        0x24 => 0xFF50, // Home
        0x25 => 0xFF51, // Left
        0x26 => 0xFF52, // Up
        0x27 => 0xFF53, // Right
        0x28 => 0xFF54, // Down
        0x2C => 0xFF61, // Print
        0x2D => 0xFF63, // Insert
        0x2E => 0xFFFF, // Delete
        0x5B => 0xFFEB, // Super_L
        0x5C => 0xFFEC, // Super_R
        0x5D => 0xFF67, // Menu
        0x6A => 0xFFAA, // KP_Multiply
        0x6B => 0xFFAB, // KP_Add
        0x6C => 0xFFAC, // KP_Separator
        0x6D => 0xFFAD, // KP_Subtract
        0x6E => 0xFFAE, // KP_Decimal
        0x6F => 0xFFAF, // KP_Divide
        0x90 => 0xFF7F, // Num_Lock
        0x91 => 0xFF14, // Scroll_Lock
        0xA0 => 0xFFE1, // Shift_L
        0xA1 => 0xFFE2, // Shift_R
        0xA2 => 0xFFE3, // Control_L
        0xA3 => 0xFFE4, // Control_R
        0xA4 => 0xFFE9, // Alt_L
        0xA5 => 0xFFEA, // Alt_R
        // Browser keys (XF86)
        0xA6 => 0x1008FF26, // Back
        0xA7 => 0x1008FF27, // Forward
        0xA8 => 0x1008FF73, // Reload
        0xA9 => 0x1008FF28, // Stop
        0xAA => 0x1008FF1B, // Search
        0xAB => 0x1008FF30, // Favorites
        0xAC => 0x1008FF18, // HomePage
        // Media keys (XF86)
        0xAD => 0x1008FF12, // AudioMute
        0xAE => 0x1008FF11, // AudioLowerVolume
        0xAF => 0x1008FF13, // AudioRaiseVolume
        0xB0 => 0x1008FF17, // AudioNext
        0xB1 => 0x1008FF16, // AudioPrev
        0xB2 => 0x1008FF15, // AudioStop
        0xB3 => 0x1008FF14, // AudioPlay
        0xB4 => 0x1008FF19, // Mail
        0xB5 => 0x1008FF32, // AudioMedia
        0xB6 => 0x1008FF40, // Launch0
        0xB7 => 0x1008FF41, // Launch1
        // OEM punctuation (US layout, matching the labels in `keys_library`).
        0xBA => 0x003B, // ;
        0xBB => 0x003D, // =
        0xBC => 0x002C, // ,
        0xBD => 0x002D, // -
        0xBE => 0x002E, // .
        0xBF => 0x002F, // slash
        0xC0 => 0x0060, // grave
        0xDB => 0x005B, // [
        0xDC => 0x005C, // backslash
        0xDD => 0x005D, // ]
        0xDE => 0x0027, // apostrophe
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::vk_to_keysym;
    use crate::keys_library::KEYS;

    #[test]
    fn every_library_key_maps_to_a_keysym() {
        for k in KEYS {
            assert!(
                vk_to_keysym(k.vk).is_some(),
                "no keysym for {} (VK 0x{:02X})",
                k.name,
                k.vk
            );
        }
    }

    #[test]
    fn known_mappings() {
        assert_eq!(vk_to_keysym(0x41), Some(0x0061)); // A -> 'a'
        assert_eq!(vk_to_keysym(0x70), Some(0xFFBE)); // F1
        assert_eq!(vk_to_keysym(0x87), Some(0xFFD5)); // F24
        assert_eq!(vk_to_keysym(0x0D), Some(0xFF0D)); // Return
        assert_eq!(vk_to_keysym(0x25), Some(0xFF51)); // Left
        assert_eq!(vk_to_keysym(0xAE), Some(0x1008FF11)); // Volume Down
        assert_eq!(vk_to_keysym(0x69), Some(0xFFB9)); // Numpad 9
        assert_eq!(vk_to_keysym(0x07), None); // unassigned
    }
}
