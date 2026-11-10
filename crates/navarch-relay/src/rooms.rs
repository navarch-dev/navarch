use super::*;
pub(super) struct Room {
    host: MachineId,
    worker: Option<MachineId>,
    pub(super) expires: Instant,
    pub(super) reusable: Instant,
    step: u8,
    pub(super) closed: bool,
}
impl Registry {
    // The caller supplies monotonic time, including in deterministic tests.
    pub(super) fn expire_rooms(&mut self, now: Instant) {
        let rooms: Vec<_> = self
            .rooms
            .iter()
            .filter(|(_, r)| !r.closed && now >= r.expires)
            .map(|(n, _)| n.clone())
            .collect();
        for room in rooms {
            self.close_room(&room, "expired");
        }
        self.rooms.retain(|_, r| now < r.reusable);
    }
    pub(super) fn close_for(&mut self, id: &MachineId) {
        let rooms: Vec<_> = self
            .rooms
            .iter()
            .filter(|(_, r)| &r.host == id || r.worker.as_ref() == Some(id))
            .map(|(n, _)| n.clone())
            .collect();
        for room in rooms {
            self.close_room(&room, "disconnected");
        }
    }
    fn close_room(&mut self, name: &str, outcome: &'static str) {
        if let Some(r) = self.rooms.get_mut(name).filter(|r| !r.closed) {
            r.closed = true;
            tracing::info!(event = "room_closed", machine = %r.host, outcome, "relay room closed");
            for id in std::iter::once(&r.host).chain(r.worker.iter()) {
                if let Some(c) = self.connections.get(id) {
                    emit(&c.output, RelayResponse::RoomClosed);
                }
            }
        }
    }
    pub(super) fn room_request(
        &mut self,
        id: &MachineId,
        out: &Output,
        request: RelayRequest,
        now: Instant,
    ) -> bool {
        self.expire_rooms(now);
        let name = match &request {
            RelayRequest::RoomOpen { room }
            | RelayRequest::RoomJoin { room }
            | RelayRequest::RoomData { room, .. }
            | RelayRequest::RoomFail { room } => room.clone(),
            _ => return false,
        };
        if name.len() != 4 || !name.bytes().all(|b| b.is_ascii_digit()) {
            return false;
        }
        if let RelayRequest::RoomOpen { .. } = request {
            if self.rooms.contains_key(&name) {
                return emit(out, RelayResponse::RoomBusy);
            }
            // One concurrent attempt per authenticated identity.
            if self
                .rooms
                .values()
                .any(|r| !r.closed && (&r.host == id || r.worker.as_ref() == Some(id)))
            {
                return false;
            }
            self.rooms.insert(
                name,
                Room {
                    host: id.clone(),
                    worker: None,
                    expires: now + Duration::from_secs(600),
                    reusable: now + Duration::from_secs(900),
                    step: 1,
                    closed: false,
                },
            );
            tracing::info!(event = "room_opened", machine = %id, "relay room opened");
            return emit(out, RelayResponse::RoomOpened);
        }
        let Some(room) = self.rooms.get_mut(&name) else {
            return emit(out, RelayResponse::RoomClosed);
        };
        if room.closed {
            return emit(out, RelayResponse::RoomClosed);
        }
        match request {
            RelayRequest::RoomJoin { .. } => {
                if room.worker.is_some() || &room.host == id {
                    self.close_room(&name, "invalid_join");
                    return emit(out, RelayResponse::RoomClosed);
                }
                room.worker = Some(id.clone());
                let Some(host) = self.connections.get(&room.host) else {
                    self.close_room(&name, "offline");
                    return false;
                };
                emit(&host.output, RelayResponse::RoomReady { peer: id.clone() })
                    && emit(
                        out,
                        RelayResponse::RoomReady {
                            peer: room.host.clone(),
                        },
                    )
            }
            RelayRequest::RoomData { step, payload, .. } => {
                let worker = room.worker.as_ref();
                let expected = if step % 2 == 1 {
                    Some(&room.host)
                } else {
                    worker
                };
                if expected != Some(id)
                    || step != room.step
                    || worker.is_none()
                    || B64.decode(&payload).map_or(true, |b| b.len() > 4096)
                {
                    self.close_room(&name, "invalid_step");
                    return false;
                }
                let to = if step % 2 == 1 {
                    worker
                } else {
                    Some(&room.host)
                };
                let Some(c) = to.and_then(|to| self.connections.get(to)) else {
                    self.close_room(&name, "offline");
                    return false;
                };
                room.step += 1;
                let sent = emit(&c.output, RelayResponse::RoomData { step, payload });
                if step == 6 && sent {
                    room.closed = true;
                    tracing::info!(event = "room_closed", machine = %room.host, outcome = "completed", "relay room closed");
                } else if !sent {
                    self.close_room(&name, "overloaded");
                }
                sent
            }
            RelayRequest::RoomFail { .. } => {
                if &room.host != id && room.worker.as_ref() != Some(id) {
                    return false;
                }
                self.close_room(&name, "failed");
                true
            }
            _ => false,
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    fn endpoint(
        registry: &mut Registry,
    ) -> (MachineId, Output, mpsc::Receiver<Option<RelayResponse>>) {
        let id: MachineId = navarch_crypto::Identity::generate()
            .machine_id()
            .parse()
            .unwrap();
        let (output, input) = mpsc::channel(64);
        let (stop, _) = tokio::sync::watch::channel(false);
        registry.connections.insert(
            id.clone(),
            Connection {
                token: 0,
                output: output.clone(),
                stop,
            },
        );
        (id, output, input)
    }
    #[test]
    fn twenty_thousand_sequential_pairing_rooms_with_advancing_monotonic_time() {
        let mut registry = Registry::default();
        let (host, hout, mut hin) = endpoint(&mut registry);
        let (worker, wout, mut win) = endpoint(&mut registry);
        let introduction = || navarch_crypto::PairInfo {
            identity: navarch_crypto::Identity::generate().public_key(),
            static_key: navarch_crypto::StaticKey::generate().public_key(),
            display_name: "stress test".into(),
        };
        let host_info = introduction();
        let worker_info = introduction();
        let start = Instant::now();
        for n in 0..20000 {
            let now = start + Duration::from_secs(n);
            let name = format!("{:04}", n % 10000);
            assert!(registry.room_request(
                &host,
                &hout,
                RelayRequest::RoomOpen { room: name.clone() },
                now
            ));
            assert!(matches!(
                hin.try_recv().unwrap(),
                Some(RelayResponse::RoomOpened)
            ));
            assert!(registry.room_request(
                &worker,
                &wout,
                RelayRequest::RoomJoin { room: name.clone() },
                now
            ));
            assert!(matches!(
                hin.try_recv().unwrap(),
                Some(RelayResponse::RoomReady { .. })
            ));
            assert!(matches!(
                win.try_recv().unwrap(),
                Some(RelayResponse::RoomReady { .. })
            ));
            let code = navarch_crypto::PairCode::parse(&format!("{name}-123456")).unwrap();
            let mut hub_pair = navarch_crypto::Pairing::new(
                navarch_crypto::PairRole::Hub,
                &code,
                host_info.clone(),
                n,
            )
            .unwrap();
            let mut worker_pair = navarch_crypto::Pairing::new(
                navarch_crypto::PairRole::Worker,
                &code,
                worker_info.clone(),
                n,
            )
            .unwrap();
            let mut message = hub_pair.start(n).unwrap();
            for step in 1..=6 {
                let (id, out, input) = if step % 2 == 1 {
                    (&host, &hout, &mut win)
                } else {
                    (&worker, &wout, &mut hin)
                };
                assert!(registry.room_request(
                    id,
                    out,
                    RelayRequest::RoomData {
                        room: name.clone(),
                        step,
                        payload: B64.encode(&message)
                    },
                    now
                ));
                let Some(RelayResponse::RoomData {
                    step: received,
                    payload,
                }) = input.try_recv().unwrap()
                else {
                    panic!()
                };
                assert_eq!(received, step);
                let pair = if step % 2 == 1 {
                    &mut worker_pair
                } else {
                    &mut hub_pair
                };
                let next = pair.receive(&B64.decode(payload).unwrap(), n).unwrap();
                if step < 6 {
                    message = next.unwrap();
                } else {
                    assert!(next.is_none());
                }
            }
            assert_eq!(hub_pair.peer().unwrap(), &worker_info);
            assert_eq!(worker_pair.peer().unwrap(), &host_info);
            assert!(registry.rooms[&name].closed);
            assert!(registry.rooms.len() <= 900);
        }
    }
    #[test]
    fn tombstones_end_fifteen_minutes_after_creation_for_every_outcome() {
        for outcome in ["completed", "failed", "disconnected", "expired"] {
            let mut registry = Registry::default();
            let (host, out, mut input) = endpoint(&mut registry);
            let start = Instant::now();
            let open = || RelayRequest::RoomOpen {
                room: "1234".into(),
            };
            assert!(registry.room_request(&host, &out, open(), start));
            input.try_recv().unwrap();
            if outcome == "expired" {
                registry.expire_rooms(start + Duration::from_secs(600));
            } else {
                registry.close_room("1234", outcome);
            }
            input.try_recv().unwrap();
            for elapsed in [
                Duration::from_secs(600),
                Duration::from_secs(899),
                Duration::from_secs(900) - Duration::from_nanos(1),
            ] {
                assert!(registry.room_request(&host, &out, open(), start + elapsed));
                assert!(matches!(
                    input.try_recv().unwrap(),
                    Some(RelayResponse::RoomBusy)
                ));
            }
            assert!(registry.room_request(&host, &out, open(), start + Duration::from_secs(900)));
            assert!(matches!(
                input.try_recv().unwrap(),
                Some(RelayResponse::RoomOpened)
            ));
        }
    }
}
