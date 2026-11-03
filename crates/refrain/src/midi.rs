//! MIDI input from keyboards and other controllers.

use std::time::Instant;

use midir::{Ignore, MidiInput, MidiInputConnection};
use refrain_engine::Event;

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

/// An open MIDI input. Events stop arriving when it is dropped.
pub struct Connection {
    pub port_name: String,
    _connection: MidiInputConnection<()>,
}

/// Connects to a MIDI input and sends what it plays to `events`, and, with
/// the moment it arrived, to `record` if given.
///
/// `select` is a port index or part of a port name. Without it, the first
/// port is used. Returns `None` when there is no MIDI input at all.
pub fn connect(
    select: Option<&str>,
    mut events: rtrb::Producer<Event>,
    mut record: Option<rtrb::Producer<(Instant, Event)>>,
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
            if let Some(event) = Event::from_midi(bytes) {
                let at = Instant::now();
                // When a queue is full the event is dropped rather than
                // blocking the MIDI thread.
                let _ = events.push(event);
                if let Some(record) = record.as_mut() {
                    let _ = record.push((at, event));
                }
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

    /// Plays into a virtual MIDI port and checks the events come out of the
    /// queue the audio thread reads.
    #[test]
    fn events_from_a_midi_port_reach_the_queue() {
        let name = format!("refrain-test-{}", std::process::id());
        let mut port = MidiOutput::new("refrain-test")
            .unwrap()
            .create_virtual(&name)
            .unwrap();
        let (tx, mut rx) = rtrb::RingBuffer::new(16);
        let (to_record, mut recorded) = rtrb::RingBuffer::new(16);
        let before = Instant::now();
        let connection = connect(Some(&name), tx, Some(to_record)).unwrap().unwrap();
        assert_eq!(connection.port_name, name);

        port.send(&[0x90, 60, 127]).unwrap();
        port.send(&[0xb0, 64, 127]).unwrap(); // sustain pedal down
        port.send(&[0xf8]).unwrap(); // clock: not read
        port.send(&[0x80, 60, 0]).unwrap();

        let mut received = Vec::new();
        let deadline = Instant::now() + Duration::from_secs(5);
        while received.len() < 3 && Instant::now() < deadline {
            match rx.pop() {
                Ok(note) => received.push(note),
                Err(_) => std::thread::sleep(Duration::from_millis(5)),
            }
        }
        assert_eq!(
            received,
            [
                Event::NoteOn {
                    channel: 0,
                    key: 60,
                    velocity: 1.0
                },
                Event::Controller {
                    channel: 0,
                    number: 64,
                    value: 127
                },
                Event::NoteOff {
                    channel: 0,
                    key: 60,
                    velocity: 0.0
                },
            ]
        );

        // The same events reach the recording queue, stamped with when they
        // arrived, in order. The MIDI thread fills that queue just after the
        // other one, so wait for it too.
        let mut stamps = Vec::new();
        while stamps.len() < 3 && Instant::now() < deadline {
            match recorded.pop() {
                Ok((at, event)) => {
                    stamps.push(at);
                    assert!(received.contains(&event));
                }
                Err(_) => std::thread::sleep(Duration::from_millis(5)),
            }
        }
        assert_eq!(stamps.len(), 3);
        assert!(stamps.is_sorted() && stamps[0] >= before);
    }
}
