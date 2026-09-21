use crate::capture::ReplayRecord;
use chrono::NaiveDateTime;
use std::{path::PathBuf, time::Instant};

const SYNTHETIC_STEP_MS: u64 = 10;
const MAX_TICK_BATCH: usize = 10_000;

#[derive(Debug, Clone)]
struct TimedReplayRecord {
    offset_ms: u64,
    record: ReplayRecord,
}

#[derive(Debug)]
pub struct ReplayController {
    pub path: PathBuf,
    records: Vec<TimedReplayRecord>,
    duration_ms: u64,
    position_ms: u64,
    cursor: usize,
    speed: f32,
    playing: bool,
    anchor_wall: Option<Instant>,
    anchor_position_ms: u64,
}

impl ReplayController {
    pub fn new(path: PathBuf, records: Vec<ReplayRecord>) -> Self {
        let records = timestamp_records(records);
        let duration_ms = records.last().map(|value| value.offset_ms).unwrap_or(0);
        Self {
            path,
            records,
            duration_ms,
            position_ms: 0,
            cursor: 0,
            speed: 1.0,
            playing: false,
            anchor_wall: None,
            anchor_position_ms: 0,
        }
    }

    pub fn len(&self) -> usize {
        self.records.len()
    }

    pub fn duration_ms(&self) -> u64 {
        self.duration_ms
    }

    pub fn position_ms(&self) -> u64 {
        self.position_ms
    }

    pub fn speed(&self) -> f32 {
        self.speed
    }

    pub fn is_playing(&self) -> bool {
        self.playing
    }

    pub fn set_speed(&mut self, speed: f32) {
        let speed = speed.clamp(0.1, 20.0);
        self.sync_position();
        self.speed = speed;
        self.reanchor();
    }

    pub fn play(&mut self) {
        if self.records.is_empty() {
            return;
        }
        if self.position_ms >= self.duration_ms && self.cursor >= self.records.len() {
            self.seek(0);
        }
        self.playing = true;
        self.reanchor();
    }

    pub fn pause(&mut self) {
        self.sync_position();
        self.playing = false;
        self.anchor_wall = None;
    }

    pub fn stop(&mut self) {
        self.playing = false;
        self.seek(0);
    }

    pub fn seek(&mut self, position_ms: u64) {
        self.position_ms = position_ms.min(self.duration_ms);
        self.cursor = self
            .records
            .partition_point(|item| item.offset_ms < self.position_ms);
        self.reanchor();
    }

    pub fn step_one(&mut self) -> Option<ReplayRecord> {
        self.pause();
        let item = self.records.get(self.cursor)?.clone();
        self.cursor += 1;
        self.position_ms = item.offset_ms;
        Some(item.record)
    }

    pub fn current_event_index(&self) -> Option<usize> {
        if self.records.is_empty() {
            None
        } else {
            Some(self.cursor.min(self.records.len().saturating_sub(1)))
        }
    }

    pub fn seek_event_index(&mut self, index: usize) -> Option<u64> {
        let item = self.records.get(index)?;
        let offset = item.offset_ms;
        self.pause();
        self.position_ms = offset;
        self.cursor = index;
        self.reanchor();
        Some(offset)
    }

    pub fn previous_event(&mut self) -> Option<u64> {
        let index = self.cursor.saturating_sub(1);
        self.seek_event_index(index)
    }

    pub fn next_event(&mut self) -> Option<u64> {
        let index = self.cursor.min(self.records.len().saturating_sub(1));
        self.seek_event_index(index)
    }

    pub fn tick(&mut self) -> Vec<ReplayRecord> {
        if !self.playing || self.records.is_empty() {
            return Vec::new();
        }

        self.sync_position();
        let mut due = Vec::new();
        while self.cursor < self.records.len()
            && self.records[self.cursor].offset_ms <= self.position_ms
            && due.len() < MAX_TICK_BATCH
        {
            due.push(self.records[self.cursor].record.clone());
            self.cursor += 1;
        }

        if self.cursor >= self.records.len() {
            self.position_ms = self.duration_ms;
            self.playing = false;
            self.anchor_wall = None;
        }

        due
    }

    fn sync_position(&mut self) {
        if !self.playing {
            return;
        }
        let Some(anchor) = self.anchor_wall else {
            return;
        };
        let elapsed_ms = anchor.elapsed().as_secs_f64() * 1_000.0 * self.speed as f64;
        let elapsed_ms = elapsed_ms.max(0.0).min(u64::MAX as f64) as u64;
        self.position_ms = self
            .anchor_position_ms
            .saturating_add(elapsed_ms)
            .min(self.duration_ms);
    }

    fn reanchor(&mut self) {
        self.anchor_position_ms = self.position_ms;
        self.anchor_wall = self.playing.then(Instant::now);
    }
}

fn timestamp_records(records: Vec<ReplayRecord>) -> Vec<TimedReplayRecord> {
    let parsed = records
        .iter()
        .map(|record| parse_timestamp_ms(&record.timestamp))
        .collect::<Vec<_>>();

    let first = parsed.iter().flatten().copied().next();
    let mut last_offset = 0u64;
    records
        .into_iter()
        .enumerate()
        .map(|(index, record)| {
            let candidate = match (first, parsed[index]) {
                (Some(first), Some(value)) if value >= first => value - first,
                _ => (index as u64).saturating_mul(SYNTHETIC_STEP_MS),
            };
            let offset_ms = candidate.max(last_offset);
            last_offset = offset_ms;
            TimedReplayRecord { offset_ms, record }
        })
        .collect()
}

fn parse_timestamp_ms(value: &str) -> Option<u64> {
    let parsed = NaiveDateTime::parse_from_str(value, "%Y-%m-%d %H:%M:%S%.f").ok()?;
    let utc = parsed.and_utc();
    let millis = utc.timestamp_millis();
    u64::try_from(millis).ok()
}

pub fn format_duration(ms: u64) -> String {
    let total_seconds = ms / 1_000;
    let millis = ms % 1_000;
    let minutes = total_seconds / 60;
    let seconds = total_seconds % 60;
    format!("{minutes:02}:{seconds:02}.{millis:03}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(timestamp: &str, byte: u8) -> ReplayRecord {
        ReplayRecord {
            timestamp: timestamp.to_owned(),
            direction: "RX".to_owned(),
            service_uuid: "service".to_owned(),
            characteristic_uuid: "char".to_owned(),
            data: vec![byte],
        }
    }

    #[test]
    fn timestamps_become_relative_offsets() {
        let controller = ReplayController::new(
            PathBuf::from("x.bmon"),
            vec![
                record("2026-09-21 12:00:00.000", 1),
                record("2026-09-21 12:00:01.250", 2),
            ],
        );
        assert_eq!(controller.duration_ms(), 1_250);
    }

    #[test]
    fn seek_updates_cursor_for_step() {
        let mut controller = ReplayController::new(
            PathBuf::from("x.bmon"),
            vec![
                record("2026-09-21 12:00:00.000", 1),
                record("2026-09-21 12:00:01.000", 2),
            ],
        );
        controller.seek(500);
        assert_eq!(controller.step_one().unwrap().data, vec![2]);
    }
    #[test]
    fn event_navigation_seeks_without_consuming() {
        let mut controller = ReplayController::new(
            PathBuf::from("x.bmon"),
            vec![
                record("2026-09-21 12:00:00.000", 1),
                record("2026-09-21 12:00:01.000", 2),
                record("2026-09-21 12:00:02.000", 3),
            ],
        );
        assert_eq!(controller.next_event(), Some(0));
        assert_eq!(controller.step_one().unwrap().data, vec![1]);
        assert_eq!(controller.next_event(), Some(1_000));
        assert_eq!(controller.previous_event(), Some(0));
    }
}
