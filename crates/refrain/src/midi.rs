//! MIDI input from keyboards and other controllers.

use midir::{Ignore, MidiInput, MidiInputConnection};
use refrain_engine::NoteEvent;

use crate::Result;

const CLIENT: &str = "refrain";

/// Names of the MIDI input ports, in the order `--midi <index>` counts them.
pub fn port_names() -> Result<Vec<String>> {
    let input = MidiInput::new(CLIENT)?;
    let names = input
        .ports()
        .iter()
        .map(|p| input.port_name(p).unwrap_or_else(|_| "(unnamed)".into()))
        .collect();
    Ok(names)
}

/// An open MIDI input. Notes stop arriving when it is dropped.
pub struct Connection {
    pub port_name: String,
    _connection: MidiInputConnection<()>,
}

/// Connects to a MIDI input and sends its notes to `notes`.
///
/// `select` is a port index or part of a port name. Without it, the first
/// port is used. Returns `None` when there is no MIDI input at all.
pub fn connect(
    select: Option<&str>,
    mut notes: rtrb::Producer<NoteEvent>,
) -> Result<Option<Connection>> {
    let mut input = MidiInput::new(CLIENT)?;
    input.ignore(Ignore::All);
    let ports = input.ports();
    if ports.is_empty() {
        if select.is_some() {
            return Err("no MIDI input ports".into());
        }
        return Ok(None);
    }
    let names: Vec<String> = ports
        .iter()
        .map(|p| input.port_name(p).unwrap_or_default())
        .collect();
    let index = match select {
        None => 0,
        Some(s) => match s.parse::<usize>() {
            Ok(i) if i < ports.len() => i,
            Ok(i) => return Err(format!("there is no MIDI input {i}").into()),
            Err(_) => {
                let wanted = s.to_lowercase();
                names
                    .iter()
                    .position(|n| n.to_lowercase().contains(&wanted))
                    .ok_or_else(|| format!("no MIDI input matches {s:?}"))?
            }
        },
    };
    let connection = input.connect(
        &ports[index],
        "refrain input",
        move |_timestamp, bytes, _| {
            if let Some(note) = NoteEvent::from_midi(bytes) {
                // When the queue is full the note is dropped rather than
                // blocking the MIDI thread.
                let _ = notes.push(note);
            }
        },
        (),
    )?;
    Ok(Some(Connection {
        port_name: names[index].clone(),
        _connection: connection,
    }))
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;
    use midir::MidiOutput;
    use midir::os::unix::VirtualOutput;
    use std::time::{Duration, Instant};

    /// Sends notes from a virtual MIDI port and checks they come out of the
    /// queue the audio thread reads.
    #[test]
    fn notes_from_a_midi_port_reach_the_queue() {
        let name = format!("refrain-test-{}", std::process::id());
        let mut port = MidiOutput::new("refrain-test")
            .unwrap()
            .create_virtual(&name)
            .unwrap();
        let (tx, mut rx) = rtrb::RingBuffer::new(16);
        let connection = connect(Some(&name), tx).unwrap().unwrap();
        assert_eq!(connection.port_name, name);

        port.send(&[0x90, 60, 127]).unwrap();
        port.send(&[0xb0, 64, 127]).unwrap(); // sustain pedal: not a note
        port.send(&[0x80, 60, 0]).unwrap();

        let mut received = Vec::new();
        let deadline = Instant::now() + Duration::from_secs(5);
        while received.len() < 2 && Instant::now() < deadline {
            match rx.pop() {
                Ok(note) => received.push(note),
                Err(_) => std::thread::sleep(Duration::from_millis(5)),
            }
        }
        assert_eq!(
            received,
            [
                NoteEvent::On {
                    channel: 0,
                    key: 60,
                    velocity: 1.0
                },
                NoteEvent::Off {
                    channel: 0,
                    key: 60,
                    velocity: 0.0
                },
            ]
        );
    }
}
