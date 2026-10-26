//! Receivers added and removed from the widget (ADR 0032): the USB HID
//! devices no `[[receiver]]` block matches, the block written for one, and
//! the edits of `hub.toml`.
//!
//! The hub never adopts a device on its own: it grabs what it forwards, and
//! adopting any HID that shows up would capture a maintenance keyboard.

use std::collections::BTreeMap;

use anyhow::{anyhow, bail, Context, Result};
use proto::descriptor::{DescriptorInfo, USAGE_PAGE_GENERIC_DESKTOP};
use proto::{DeviceKind, UnclaimedReceiver};
use toml_edit::{value, ArrayOfTables, DocumentMut, Item, Table};

use crate::config::{HubConfig, ReceiverMatch, MAX_RECEIVERS};

/// One hidraw interface on the board that no block matches.
#[derive(Debug, Clone, PartialEq)]
pub struct Candidate {
    pub vid: u16,
    pub pid: u16,
    pub serial: Option<String>,
    pub name: String,
    /// `usb-3f980000.usb-1.3/input0`: the part before `/input` is the
    /// USB port, which tells two identical devices apart.
    pub phys: String,
    pub descriptor: Vec<u8>,
}

/// What one interface carries: keyboard (or keypad), mouse (or pointer),
/// anything else.
pub fn kinds(info: &DescriptorInfo) -> Vec<DeviceKind> {
    let mut out: Vec<DeviceKind> = info
        .applications
        .iter()
        .map(|&(page, usage)| match (page, usage) {
            (USAGE_PAGE_GENERIC_DESKTOP, 0x06 | 0x07) => DeviceKind::Keyboard,
            (USAGE_PAGE_GENERIC_DESKTOP, 0x01 | 0x02) => DeviceKind::Mouse,
            _ => DeviceKind::Other,
        })
        .collect();
    if out.is_empty() {
        out.push(DeviceKind::Other);
    }
    out.sort();
    out.dedup();
    out
}

/// The interfaces grouped by USB device: same vid and pid, and the same
/// serial number or, without one, the same USB port.
pub fn group_unclaimed(candidates: &[Candidate]) -> Vec<UnclaimedReceiver> {
    let mut groups: BTreeMap<(u16, u16, String), UnclaimedReceiver> = BTreeMap::new();
    for c in candidates {
        let port = c.phys.split("/input").next().unwrap_or(&c.phys);
        let key = (
            c.vid,
            c.pid,
            c.serial.clone().unwrap_or_else(|| port.to_string()),
        );
        let g = groups.entry(key).or_insert_with(|| UnclaimedReceiver {
            vid: c.vid,
            pid: c.pid,
            name: c.name.clone(),
            serial: c.serial.clone(),
            interfaces: 0,
            kinds: Vec::new(),
        });
        g.interfaces = g.interfaces.saturating_add(1);
        // The interfaces of one device may be named "X", "X Mouse",
        // "X Consumer Control": keep the shortest.
        if !c.name.is_empty() && (g.name.is_empty() || c.name.len() < g.name.len()) {
            g.name = c.name.clone();
        }
        g.kinds.extend(kinds(&DescriptorInfo::parse(&c.descriptor)));
        g.kinds.sort();
        g.kinds.dedup();
    }
    groups.into_values().collect()
}

/// A block name from a product name: lowercase `[a-z0-9-]`, made unique
/// against `taken` with `-2`, `-3`...
pub fn derive_name(product: &str, taken: &[String]) -> String {
    let mut base = String::new();
    for c in product.chars() {
        if c.is_ascii_alphanumeric() {
            base.push(c.to_ascii_lowercase());
        } else if !base.ends_with('-') {
            base.push('-');
        }
    }
    let mut base = base.trim_matches('-').to_string();
    base.truncate(32);
    let base = match base.trim_end_matches('-') {
        "" => "receiver".to_string(),
        b => b.to_string(),
    };
    if !taken.contains(&base) {
        return base;
    }
    (2..)
        .map(|n| format!("{base}-{n}"))
        .find(|n| !taken.contains(n))
        .expect("an unused suffix")
}

/// The block for an unclaimed device: only its product (`pids = [pid]`),
/// `match = "receiver"`, `grab_evdev`, and the vendor's defaults.
pub fn new_block(vid: u16, pid: u16, product: &str, taken: &[String]) -> ReceiverMatch {
    ReceiverMatch {
        name: derive_name(product, taken),
        pids: vec![pid],
        grab_evdev: true,
        ..ReceiverMatch::for_vendor(vid)
    }
}

/// `[[receiver]]` text of `m`, with hex ids and a comment saying where it
/// came from.
fn block_text(m: &ReceiverMatch, product: &str) -> String {
    let pids: Vec<String> = m.pids.iter().map(|p| format!("0x{p:04x}")).collect();
    let mode = match m.mode {
        crate::config::MatchMode::Receiver => "receiver",
        crate::config::MatchMode::DjChildren => "dj-children",
    };
    // `product` goes in a comment: no newline may break out of it.
    let product: String = product.chars().filter(|c| !c.is_control()).collect();
    format!(
        "\n# Added from the widget: {vid:04x}:{pid} {product}\n\
         [[receiver]]\n\
         name = {name}\n\
         match = \"{mode}\"\n\
         vid = 0x{vid:04x}\n\
         pids = [{pids}]\n\
         grab_evdev = {grab}\n\
         force_normal_mode = {fnm}\n\
         require_hid_generic = {rhg}\n",
        vid = m.vid,
        pid = m
            .pids
            .first()
            .map(|p| format!("{p:04x}"))
            .unwrap_or_default(),
        name = toml_edit::Value::from(m.name.as_str()),
        pids = pids.join(", "),
        grab = m.grab_evdev,
        fnm = m.force_normal_mode,
        rhg = m.require_hid_generic,
    )
}

/// One past the highest table position in `doc`. toml_edit writes the
/// tables in position order, and a table without one (or with one from
/// another document) lands after the first positioned table: every table
/// added here gets `next_position`, so it goes last in the file and last
/// in its array.
fn next_position(doc: &DocumentMut) -> usize {
    fn max_in(t: &Table) -> Option<usize> {
        let inner = t.iter().filter_map(|(_, item)| match item {
            Item::Table(t) => max_in(t),
            Item::ArrayOfTables(a) => a.iter().filter_map(max_in).max(),
            _ => None,
        });
        t.position().into_iter().chain(inner).max()
    }
    max_in(doc.as_table()).map_or(0, |p| p + 1)
}

/// The `[[receiver]]` array of `doc`, created from the effective blocks
/// when the file has none (the default Logitech block) or a single
/// `[receiver]` table; every block gets its `name` written, so a later
/// removal does not shift the default `receiver-N` names (and with them the
/// exposures kept by name in settings.toml).
fn receiver_array<'a>(
    doc: &'a mut DocumentMut,
    effective: &[ReceiverMatch],
) -> Result<&'a mut ArrayOfTables> {
    let item = doc.as_table_mut().remove("receiver");
    let mut aot = match item {
        None => {
            let mut aot = ArrayOfTables::new();
            for (position, m) in (next_position(doc)..).zip(effective) {
                let text = block_text(m, "");
                let parsed: DocumentMut = text.parse()?;
                let mut t = parsed["receiver"]
                    .as_array_of_tables()
                    .and_then(|a| a.get(0))
                    .cloned()
                    .ok_or_else(|| anyhow!("rendering a receiver block"))?;
                t.decor_mut()
                    .set_prefix("\n# The default receiver, written out to add another.\n");
                t.set_position(position);
                aot.push(t);
            }
            aot
        }
        Some(Item::Table(t)) => {
            let mut aot = ArrayOfTables::new();
            aot.push(t);
            aot
        }
        Some(Item::ArrayOfTables(aot)) => aot,
        Some(_) => bail!("`receiver` in hub.toml is neither a table nor an array of tables"),
    };
    for (t, m) in aot.iter_mut().zip(effective) {
        if !t.contains_key("name") {
            t.insert("name", value(m.name.clone()));
        }
    }
    doc.as_table_mut()
        .insert("receiver", Item::ArrayOfTables(aot));
    doc["receiver"]
        .as_array_of_tables_mut()
        .ok_or_else(|| anyhow!("rebuilding [[receiver]]"))
}

/// `hub.toml` with `block` appended, keeping everything else (comments,
/// order, the other blocks). The result is validated as a config: an
/// overlap, a repeated name or too many blocks are errors.
pub fn add_to_config(text: &str, block: &ReceiverMatch, product: &str) -> Result<String> {
    let effective = HubConfig::parse(text)
        .context("the current hub.toml")?
        .receivers;
    let mut doc: DocumentMut = text.parse().context("parsing hub.toml")?;
    let parsed: DocumentMut = block_text(block, product).parse()?;
    let mut table = parsed["receiver"]
        .as_array_of_tables()
        .and_then(|a| a.get(0))
        .cloned()
        .ok_or_else(|| anyhow!("rendering the new block"))?;
    let comment = block_text(block, product)
        .lines()
        .take(2)
        .collect::<Vec<_>>()
        .join("\n");
    table.decor_mut().set_prefix(format!("{comment}\n"));
    receiver_array(&mut doc, &effective)?;
    table.set_position(next_position(&doc));
    doc["receiver"]
        .as_array_of_tables_mut()
        .ok_or_else(|| anyhow!("rebuilding [[receiver]]"))?
        .push(table);
    let out = doc.to_string();
    let cfg = HubConfig::parse(&out)?;
    if cfg.receivers.len() > MAX_RECEIVERS {
        bail!("at most {MAX_RECEIVERS} receivers");
    }
    Ok(out)
}

/// `hub.toml` without the block named `name`. The last block cannot go.
pub fn remove_from_config(text: &str, name: &str) -> Result<String> {
    let effective = HubConfig::parse(text)
        .context("the current hub.toml")?
        .receivers;
    if !effective.iter().any(|m| m.name == name) {
        bail!("hub.toml has no receiver named {name:?} (edited by hand? restart the hub)");
    }
    if effective.len() == 1 {
        bail!("{name:?} is the only receiver; the hub needs at least one");
    }
    let mut doc: DocumentMut = text.parse().context("parsing hub.toml")?;
    let aot = receiver_array(&mut doc, &effective)?;
    let i = aot
        .iter()
        .position(|t| t.get("name").and_then(|n| n.as_str()) == Some(name))
        .ok_or_else(|| anyhow!("no block named {name:?}"))?;
    aot.remove(i);
    let out = doc.to_string();
    HubConfig::parse(&out)?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use proto::descriptor::tests::{BOOT_KEYBOARD, UNIFYING_IFACE1, UNIFYING_IFACE2};

    fn cand(vid: u16, pid: u16, port: &str, iface: u8, desc: &[u8], name: &str) -> Candidate {
        Candidate {
            vid,
            pid,
            serial: None,
            name: name.into(),
            phys: format!("usb-3f980000.usb-{port}/input{iface}"),
            descriptor: desc.to_vec(),
        }
    }

    #[test]
    fn groups_interfaces_by_usb_device() {
        let list = [
            cand(
                0x25a7,
                0xfa09,
                "1.3",
                0,
                BOOT_KEYBOARD,
                "Compx 2.4G Receiver",
            ),
            cand(
                0x25a7,
                0xfa09,
                "1.3",
                1,
                UNIFYING_IFACE1,
                "Compx 2.4G Receiver Mouse",
            ),
            // The same model on another port is another device.
            cand(
                0x25a7,
                0xfa09,
                "1.4",
                0,
                UNIFYING_IFACE2,
                "Compx 2.4G Receiver",
            ),
            cand(0x1234, 0x0001, "1.2", 0, &[], ""),
        ];
        let g = group_unclaimed(&list);
        let summary: Vec<_> = g
            .iter()
            .map(|u| (u.vid, u.name.as_str(), u.interfaces, u.kinds.clone()))
            .collect();
        assert_eq!(
            summary,
            [
                (0x1234, "", 1, vec![DeviceKind::Other]),
                (
                    0x25a7,
                    "Compx 2.4G Receiver",
                    2,
                    vec![DeviceKind::Keyboard, DeviceKind::Mouse, DeviceKind::Other]
                ),
                (0x25a7, "Compx 2.4G Receiver", 1, vec![DeviceKind::Other]),
            ]
        );
        // A serial number groups across ports.
        let with_serial = |port| Candidate {
            serial: Some("A1".into()),
            ..cand(0x25a7, 0xfa09, port, 0, BOOT_KEYBOARD, "x")
        };
        assert_eq!(
            group_unclaimed(&[with_serial("1.3"), with_serial("1.4")]).len(),
            1
        );
    }

    #[test]
    fn names_are_derived_and_unique() {
        let taken = vec!["logitech".to_string(), "compx-2-4g-receiver".to_string()];
        assert_eq!(
            derive_name("Compx 2.4G Receiver", &[]),
            "compx-2-4g-receiver"
        );
        assert_eq!(
            derive_name("Compx 2.4G Receiver", &taken),
            "compx-2-4g-receiver-2"
        );
        assert_eq!(derive_name("  ¡Ñandú!  ", &[]), "and");
        assert_eq!(derive_name("", &[]), "receiver");
        assert_eq!(derive_name("---", &["receiver".into()]), "receiver-2");
        assert!(derive_name(&"x".repeat(80), &[]).len() <= 32);
        let b = new_block(0x25a7, 0xfa09, "Compx", &taken);
        assert_eq!(
            (b.name.as_str(), b.pids.as_slice()),
            ("compx", &[0xfa09][..])
        );
        assert!(b.grab_evdev && !b.force_normal_mode && !b.require_hid_generic);
        let l = new_block(0x046d, 0xc548, "Logitech Bolt", &taken);
        assert!(l.force_normal_mode && l.require_hid_generic);
        assert_eq!(l.pids, [0xc548]);
    }

    const COMMENTED: &str = "# my hub\nname = \"pi\"   # the name\n\n\
        [[receiver]]\nname = \"logitech\"\n# keep this comment\n\n\
        [switching]\nhysteresis_ms = 200\n";

    #[test]
    fn adding_keeps_the_rest_of_the_file() {
        let block = new_block(0x25a7, 0xfa09, "Compx 2.4G Receiver", &["logitech".into()]);
        let out = add_to_config(COMMENTED, &block, "Compx 2.4G Receiver").unwrap();
        for kept in [
            "# my hub",
            "# the name",
            "# keep this comment",
            "hysteresis_ms = 200",
        ] {
            assert!(out.contains(kept), "{kept} lost:\n{out}");
        }
        assert!(out.contains("# Added from the widget: 25a7:fa09 Compx 2.4G Receiver"));
        assert!(
            out.contains("vid = 0x25a7") && out.contains("pids = [0xfa09]"),
            "{out}"
        );
        let before = HubConfig::parse(COMMENTED).unwrap();
        let after = HubConfig::parse(&out).unwrap();
        assert_eq!(after.receivers[0], before.receivers[0]);
        assert_eq!(after.receivers[1], block);
        assert_eq!(after.switching, before.switching);

        // And removing it gives the same configuration back.
        let back = remove_from_config(&out, &block.name).unwrap();
        assert_eq!(HubConfig::parse(&back).unwrap(), before);
        assert!(back.contains("# keep this comment"), "{back}");
    }

    fn names(text: &str) -> Vec<String> {
        HubConfig::parse(text)
            .unwrap()
            .receivers
            .into_iter()
            .map(|r| r.name)
            .collect()
    }

    /// The new block is the last one of the array, wherever the blocks and
    /// the other tables are in the file (the order is the receiver index
    /// after a restart).
    #[test]
    fn the_new_block_goes_last() {
        let block = new_block(
            0x25a7,
            0xfa09,
            "Compx",
            &["logitech".into(), "areson".into()],
        );
        // Like the hub's real file.
        let real = "# RawKVM hub configuration\nname = \"rawkvm-hub\"\n\n\
            [[receiver]]\nname = \"logitech\"\nmatch = \"receiver\"\nvid = 1133\n\
            pids = [\n    50475, # Unifying\n    50482,\n]\n\n\
            [[receiver]]\nname = \"areson\"\nvid = 0x25a7\npids = [0xfa08]\n\n\
            [switching]\nhysteresis_ms = 150\n";
        let out = add_to_config(real, &block, "Compx").unwrap();
        assert_eq!(names(&out), ["logitech", "areson", "compx"], "{out}");
        // Blocks after another table.
        let after = "[switching]\nhysteresis_ms = 150\n\n[[receiver]]\nname = \"logitech\"\n\n\
            [[receiver]]\nname = \"areson\"\nvid = 0x25a7\npids = [0xfa08]\n\n[edge]\npush_ms = 0\n";
        let out = add_to_config(after, &block, "Compx").unwrap();
        assert_eq!(names(&out), ["logitech", "areson", "compx"], "{out}");
        // The implicit default block, with other tables around.
        let implicit = "[switching]\nhysteresis_ms = 150\n[edge]\npush_ms = 0\n";
        let out = add_to_config(implicit, &block, "Compx").unwrap();
        assert_eq!(names(&out), ["receiver-0", "compx"], "{out}");
        // And twice in a row.
        let second = new_block(0x1234, 0x0001, "Other", &names(&out));
        let out = add_to_config(&out, &second, "Other").unwrap();
        assert_eq!(names(&out), ["receiver-0", "compx", "other"], "{out}");
    }

    #[test]
    fn old_and_implicit_receiver_forms() {
        let block = new_block(0x25a7, 0xfa09, "Compx", &[]);
        // No receiver key: the default block is written out first.
        let out = add_to_config("name = \"pi\"\n", &block, "Compx").unwrap();
        let cfg = HubConfig::parse(&out).unwrap();
        assert_eq!(cfg.receivers[0], HubConfig::default().receivers[0]);
        assert_eq!(cfg.receivers[1], block);
        // A single [receiver] table (0.5.0) becomes the first element.
        let old = "[receiver]\nvid = 1133\npids = [50475]\n";
        let out = add_to_config(old, &block, "Compx").unwrap();
        let cfg = HubConfig::parse(&out).unwrap();
        assert_eq!(cfg.receivers[0].pids, [50475]);
        assert_eq!(cfg.receivers[0].name, "receiver-0");
        assert!(out.contains("name = \"receiver-0\""), "{out}");
    }

    #[test]
    fn default_names_are_written_before_removing() {
        // receiver-1 is removed; receiver-2 must keep its name.
        let text = "[[receiver]]\n[[receiver]]\nvid = 1\n[[receiver]]\nvid = 2\n";
        let out = remove_from_config(text, "receiver-1").unwrap();
        let names: Vec<_> = HubConfig::parse(&out)
            .unwrap()
            .receivers
            .into_iter()
            .map(|r| (r.name, r.vid))
            .collect();
        assert_eq!(
            names,
            [("receiver-0".into(), 0x046d), ("receiver-2".into(), 2)]
        );
    }

    #[test]
    fn refusals() {
        let one = "[[receiver]]\nname = \"logitech\"\n";
        let err = |r: Result<String>| format!("{:#}", r.unwrap_err());
        assert!(err(remove_from_config(one, "logitech")).contains("only receiver"));
        assert!(err(remove_from_config(one, "other")).contains("no receiver named"));
        // A Logitech product already covered by the default block.
        let dup = new_block(0x046d, 0xc52b, "Unifying", &["logitech".into()]);
        assert!(err(add_to_config(one, &dup, "Unifying")).contains("overlap"));
        // A name already taken (new_block avoids it; a hand-made one may not).
        let same = ReceiverMatch {
            name: "logitech".into(),
            ..new_block(0x25a7, 0xfa09, "x", &[])
        };
        assert!(err(add_to_config(one, &same, "x")).contains("both named"));
        let many: String = (1..=16)
            .map(|i| format!("[[receiver]]\nvid = {i}\n"))
            .collect();
        let b = new_block(0x25a7, 0xfa09, "x", &[]);
        assert!(err(add_to_config(&many, &b, "x")).contains("at most"));
    }
}
