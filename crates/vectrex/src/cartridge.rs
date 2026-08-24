//! The cartridge header: what a game tells the BIOS about itself.
//!
//! Every cartridge opens with a fixed structure the BIOS reads before running
//! any of its code — a copyright line, a tune to play, and the title to draw,
//! with the size and screen position it should be drawn at. So a title screen
//! is partly *data*, which is why it can be read without executing anything.

/// One line of the title, with the size and position the BIOS draws it at.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Title {
    pub text: String,
    /// Glyph height and width, signed deflection units.
    pub height: i8,
    pub width: i8,
    /// Where the line starts, relative to screen centre.
    pub y: i8,
    pub x: i8,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Header {
    /// The "g GCE 1982" line. Absent on images that are not cartridges.
    pub copyright: Option<String>,
    /// Address of the tune the BIOS plays on startup.
    pub music: u16,
    pub titles: Vec<Title>,
}

/// Strings in the header end with the high bit set rather than a NUL.
const TERMINATOR: u8 = 0x80;

fn string_at(rom: &[u8], from: usize) -> Option<(String, usize)> {
    let end = rom.iter().skip(from).position(|&b| b == TERMINATOR)? + from;
    let text = rom.get(from..end)?;
    if text.iter().any(|&b| !(0x20..0x7F).contains(&b)) {
        return None;
    }
    Some((String::from_utf8_lossy(text).into_owned(), end + 1))
}

/// Read the header from the start of a cartridge image.
///
/// Returns what it could parse rather than failing: an image that is not a
/// cartridge — the BIOS itself, say — simply has no titles.
pub fn parse(rom: &[u8]) -> Header {
    let mut header = Header::default();
    let mut at = 0;

    if rom.first() == Some(&b'g') {
        match string_at(rom, 0) {
            Some((text, next)) => {
                header.copyright = Some(text);
                at = next;
            }
            None => return header,
        }
        // The music pointer follows the copyright line, high byte first.
        let Some(&high) = rom.get(at) else { return header };
        let Some(&low) = rom.get(at + 1) else { return header };
        header.music = u16::from(high) << 8 | u16::from(low);
        at += 2;
    }

    // Then one entry per title line, ended by a zero where a height would be.
    while let Some(&byte) = rom.get(at) {
        if byte == 0 {
            break;
        }
        let Some(fields) = rom.get(at..at + 4) else { break };
        let Some((text, next)) = string_at(rom, at + 4) else { break };
        header.titles.push(Title {
            text,
            height: fields[0] as i8,
            width: fields[1] as i8,
            y: fields[2] as i8,
            x: fields[3] as i8,
        });
        at = next;
    }
    header
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::{load, rom_dir};

    #[test]
    fn a_real_cartridge_names_itself() {
        let rom = load(rom_dir().join("Armor Attack (1982).vec")).expect("a cartridge image");
        let header = parse(&rom);

        let copyright = header.copyright.expect("every cartridge opens with one");
        assert!(copyright.starts_with("g GCE"), "got {copyright:?}");
        assert_eq!(header.titles.len(), 1);
        assert_eq!(header.titles[0].text, "ARMOR ATTACK");
        // Drawn left of centre and above it, at the BIOS's usual glyph size.
        assert!(header.titles[0].x < 0 && header.titles[0].y > 0);
        assert!(header.titles[0].height < 0, "height counts downward");
    }

    #[test]
    fn a_multi_line_title_yields_a_line_each() {
        let rom = load(rom_dir().join("Mine Storm II (1983).vec")).expect("a cartridge image");
        let header = parse(&rom);
        let lines: Vec<&str> = header.titles.iter().map(|t| t.text.as_str()).collect();
        assert_eq!(lines, vec!["MINE", "STORM", "II"]);
        // Stacked down the screen, so each line sits below the one before.
        for pair in header.titles.windows(2) {
            assert!(pair[1].y < pair[0].y, "lines should descend: {lines:?}");
        }
    }

    #[test]
    fn an_image_that_is_not_a_cartridge_parses_to_nothing() {
        // The BIOS image starts with code, not a header.
        let rom = load(rom_dir().join("Mine Storm (1982).vec")).expect("the BIOS image");
        let header = parse(&rom);
        assert!(header.copyright.is_none());
        assert!(header.titles.is_empty());
    }

    #[test]
    fn a_truncated_header_does_not_panic() {
        assert_eq!(parse(b"g GCE 1982"), Header::default());
        assert!(parse(b"").titles.is_empty());
        assert!(parse(&[0x67, 0x80]).titles.is_empty());
    }
}
