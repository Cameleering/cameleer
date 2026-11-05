//! The Kafka consumer: `consumersCount` worker threads, each owning a client and (with a
//! `groupId`) a consumer group membership.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use cameleer_core::component::support::{process_consumer_exchange, BackgroundThreads, StopFlag};
use cameleer_core::context::WeakContext;
use cameleer_core::error::{CamelError, Result};
use cameleer_core::message::Exchange;
use cameleer_core::regex::Regex;
use cameleer_core::spi::{Processor, Service};
use cameleer_core::value::Value;

use super::client::KafkaClient;
use super::protocol::*;
use super::{headers, is_filtered_header, manual_registry, CommitOnStop, KafkaConfig, OffsetReset, PendingCommits, SeekTo};
use crate::net::unique_id;

pub(crate) struct KafkaConsumer {
    cfg: KafkaConfig,
    ctx: WeakContext,
    processor: Arc<dyn Processor>,
    logger: String,
    threads: BackgroundThreads,
}

impl KafkaConsumer {
    pub(crate) fn new(cfg: KafkaConfig, ctx: WeakContext, processor: Arc<dyn Processor>, logger: String) -> Self {
        KafkaConsumer { cfg, ctx, processor, logger, threads: BackgroundThreads::new() }
    }
}

impl Service for KafkaConsumer {
    fn start(&self) -> Result<()> {
        if self.threads.is_running() {
            return Ok(());
        }
        let pattern = if self.cfg.topic_is_pattern {
            Some(Regex::new(&format!("^(?:{})$", self.cfg.topic)).map_err(|e| CamelError::illegal_argument(format!("Invalid topic pattern {}: {}", self.cfg.topic, e.message)))?)
        } else {
            None
        };
        for i in 0..self.cfg.consumers_count {
            let mut w = Worker::new(self.cfg.clone(), self.ctx.clone(), self.processor.clone(), self.logger.clone(), i, pattern.clone())?;
            self.threads.spawn(&format!("kafka-consumer-{}-{}", self.cfg.topic, i), move |flag| w.run(flag))?;
        }
        cameleer_core::camel_info!("kafka", "Kafka consumer started on {} ({} thread(s))", self.cfg.topic, self.cfg.consumers_count);
        Ok(())
    }
    fn stop(&self) -> Result<()> {
        self.threads.stop();
        Ok(())
    }
}

type Tp = (String, i32);

struct Membership {
    coordinator: i32,
    generation: i32,
}

struct Worker {
    cfg: KafkaConfig,
    ctx: WeakContext,
    processor: Arc<dyn Processor>,
    logger: String,
    pattern: Option<Regex>,
    client: KafkaClient,
    consumer_id: String,
    member_id: String,
    membership: Option<Membership>,
    needs_assignment: bool,
    first_assignment: bool,
    assignment: Vec<Tp>,
    positions: BTreeMap<Tp, i64>,
    /// Offsets processed but not yet committed (next offset to read).
    uncommitted: HashMap<Tp, i64>,
    manual: PendingCommits,
    last_commit: Instant,
    last_heartbeat: Instant,
}

impl Worker {
    fn new(cfg: KafkaConfig, ctx: WeakContext, processor: Arc<dyn Processor>, logger: String, index: usize, pattern: Option<Regex>) -> Result<Worker> {
        let client = KafkaClient::new(cfg.client_config(&format!("-{}", index))?);
        Ok(Worker {
            cfg,
            ctx,
            processor,
            logger,
            pattern,
            client,
            consumer_id: unique_id(),
            member_id: String::new(),
            membership: None,
            needs_assignment: true,
            first_assignment: true,
            assignment: Vec::new(),
            positions: BTreeMap::new(),
            uncommitted: HashMap::new(),
            manual: Arc::new(Mutex::new(HashMap::new())),
            last_commit: Instant::now(),
            last_heartbeat: Instant::now(),
        })
    }

    fn fetch_wait(&self) -> Duration {
        let mut w = self.cfg.fetch_wait_max.min(self.cfg.poll_timeout);
        if self.cfg.group_id.is_some() {
            w = w.min(self.cfg.heartbeat_interval);
        }
        w.max(Duration::from_millis(1))
    }

    fn run(&mut self, flag: Arc<StopFlag>) {
        if self.cfg.allow_manual_commit {
            manual_registry().lock().unwrap().insert(self.consumer_id.clone(), self.manual.clone());
        }
        let mut failures = 0u32;
        while !flag.is_stopped() {
            match self.step(&flag) {
                Ok(()) => failures = 0,
                Err(e) => {
                    failures += 1;
                    if !flag.is_stopped() {
                        cameleer_core::camel_warn!(&self.logger, "Kafka consumer error on {}: {} - {}", self.cfg.topic, e.kind, e.message);
                    }
                    self.client.close();
                    self.membership = None;
                    self.needs_assignment = true;
                    let backoff = Duration::from_millis((100u64 << failures.min(5)).min(3000));
                    if flag.sleep(backoff) {
                        break;
                    }
                }
            }
        }
        self.flush_manual();
        if self.cfg.auto_commit_enable && self.cfg.auto_commit_on_stop != CommitOnStop::None {
            let _ = self.commit();
        }
        if let (Some(m), Some(group)) = (&self.membership, &self.cfg.group_id) {
            let req = LeaveGroupRequest { group_id: group.clone(), member_id: self.member_id.clone() };
            let _ = self.client.leave_group(m.coordinator, &req);
        }
        self.client.close();
        manual_registry().lock().unwrap().remove(&self.consumer_id);
    }

    fn step(&mut self, flag: &StopFlag) -> Result<()> {
        if self.needs_assignment {
            self.assign(flag)?;
            if self.needs_assignment {
                return Ok(());
            }
        }
        if self.membership.is_some() && self.last_heartbeat.elapsed() >= self.cfg.heartbeat_interval {
            self.heartbeat()?;
            if self.needs_assignment {
                return Ok(());
            }
        }
        if self.assignment.is_empty() {
            flag.sleep(self.fetch_wait());
            return Ok(());
        }
        self.poll(flag)?;
        self.flush_manual();
        if self.cfg.auto_commit_enable && self.last_commit.elapsed() >= self.cfg.auto_commit_interval {
            self.commit()?;
        }
        Ok(())
    }

    fn subscribed_topics(&mut self) -> Result<Vec<String>> {
        match &self.pattern {
            None => Ok(vec![self.cfg.topic.clone()]),
            Some(re) => {
                let re = re.clone();
                let mut t: Vec<String> = self.client.list_topics()?.into_iter().filter(|t| re.is_match(t)).collect();
                t.sort();
                Ok(t)
            }
        }
    }

    // -----------------------------------------------------------------------
    // assignment
    // -----------------------------------------------------------------------

    fn assign(&mut self, flag: &StopFlag) -> Result<()> {
        let topics = self.subscribed_topics()?;
        let assignment: Vec<Tp> = match self.cfg.group_id.clone() {
            None => {
                let mut out = Vec::new();
                for t in &topics {
                    for p in self.client.partitions(t, true)? {
                        out.push((t.clone(), p.partition));
                    }
                }
                out
            }
            Some(group) => match self.join(&group, &topics, flag)? {
                Some(a) => a,
                None => return Ok(()), // stopped or retry later
            },
        };
        // resolve positions
        let mut positions = BTreeMap::new();
        let committed = match (&self.membership, &self.cfg.group_id) {
            (Some(m), Some(group)) if !assignment.is_empty() => {
                let mut by_topic: BTreeMap<String, Vec<i32>> = BTreeMap::new();
                for (t, p) in &assignment {
                    by_topic.entry(t.clone()).or_default().push(*p);
                }
                let req: Vec<(String, Vec<i32>)> = by_topic.into_iter().collect();
                self.client.offset_fetch(m.coordinator, group, &req)?
            }
            _ => HashMap::new(),
        };
        let seek = if self.first_assignment { self.cfg.seek_to } else { None };
        let mut need_reset: BTreeMap<String, Vec<i32>> = BTreeMap::new();
        let mut need_seek: BTreeMap<String, Vec<i32>> = BTreeMap::new();
        for tp in &assignment {
            if seek.is_some() {
                need_seek.entry(tp.0.clone()).or_default().push(tp.1);
                continue;
            }
            // a position we already had for a partition we keep is more recent than the commit
            if let Some(pos) = self.positions.get(tp) {
                positions.insert(tp.clone(), *pos);
                continue;
            }
            match committed.get(tp) {
                Some(off) if *off >= 0 => {
                    positions.insert(tp.clone(), *off);
                }
                _ => need_reset.entry(tp.0.clone()).or_default().push(tp.1),
            }
        }
        for (t, parts) in need_seek {
            let ts = if seek == Some(SeekTo::Beginning) { EARLIEST_TIMESTAMP } else { LATEST_TIMESTAMP };
            for (p, off) in self.client.list_offsets(&t, &parts, ts)? {
                positions.insert((t.clone(), p), off);
            }
        }
        for (t, parts) in need_reset {
            let ts = match self.cfg.auto_offset_reset {
                OffsetReset::Earliest => EARLIEST_TIMESTAMP,
                OffsetReset::Latest => LATEST_TIMESTAMP,
                OffsetReset::None => {
                    return Err(CamelError::new("NoOffsetForPartitionException", format!("Undefined offset with no reset policy for partitions of {}: {:?}", t, parts)));
                }
            };
            for (p, off) in self.client.list_offsets(&t, &parts, ts)? {
                positions.insert((t.clone(), p), off);
            }
        }
        // keep uncommitted offsets only for partitions still owned
        self.uncommitted.retain(|tp, _| assignment.contains(tp));
        cameleer_core::camel_debug!(&self.logger, "Kafka consumer {} assigned {:?}", self.member_id, assignment);
        self.assignment = assignment;
        self.positions = positions;
        self.first_assignment = false;
        self.needs_assignment = false;
        self.last_heartbeat = Instant::now();
        Ok(())
    }

    /// Join the group and sync; returns the assignment, or None to retry later.
    fn join(&mut self, group: &str, topics: &[String], flag: &StopFlag) -> Result<Option<Vec<Tp>>> {
        let coordinator = match &self.membership {
            Some(m) => m.coordinator,
            None => self.client.find_coordinator(group)?,
        };
        for _ in 0..20 {
            if flag.is_stopped() {
                return Ok(None);
            }
            let req = JoinGroupRequest {
                group_id: group.to_string(),
                session_timeout_ms: self.cfg.session_timeout.as_millis() as i32,
                rebalance_timeout_ms: self.cfg.max_poll_interval.as_millis() as i32,
                member_id: self.member_id.clone(),
                protocol_type: "consumer".into(),
                protocols: vec![("range".into(), encode_subscription(topics))],
            };
            let resp = self.client.join_group(coordinator, &req)?;
            match resp.error_code {
                errors::NONE => {}
                errors::UNKNOWN_MEMBER_ID => {
                    self.member_id.clear();
                    continue;
                }
                // KIP-394: rejoin with the member id the coordinator assigned
                errors::MEMBER_ID_REQUIRED if !resp.member_id.is_empty() => {
                    self.member_id = resp.member_id.clone();
                    continue;
                }
                errors::REBALANCE_IN_PROGRESS => continue,
                errors::NOT_COORDINATOR | errors::COORDINATOR_NOT_AVAILABLE | errors::COORDINATOR_LOAD_IN_PROGRESS => {
                    self.membership = None;
                    return Err(kafka_error(resp.error_code, &format!("JoinGroup {}", group)));
                }
                code => return Err(kafka_error(code, &format!("JoinGroup {}", group))),
            }
            self.member_id = resp.member_id.clone();
            let assignments = if resp.leader == resp.member_id {
                let members: Vec<(String, Vec<String>)> =
                    resp.members.iter().map(|(id, meta)| (id.clone(), decode_subscription(meta).unwrap_or_default())).collect();
                let mut all: Vec<String> = members.iter().flat_map(|(_, t)| t.iter().cloned()).collect();
                all.sort();
                all.dedup();
                let mut counts = Vec::new();
                for t in &all {
                    if let Ok(p) = self.client.partitions(t, true) {
                        counts.push((t.clone(), p.len()));
                    }
                }
                range_assign(&members, &counts).into_iter().map(|(m, a)| (m, encode_assignment(&a))).collect()
            } else {
                Vec::new()
            };
            let sync = SyncGroupRequest { group_id: group.to_string(), generation_id: resp.generation_id, member_id: self.member_id.clone(), assignments };
            let sresp = self.client.sync_group(coordinator, &sync, self.cfg.max_poll_interval)?;
            match sresp.error_code {
                errors::NONE => {}
                errors::REBALANCE_IN_PROGRESS | errors::ILLEGAL_GENERATION => continue,
                errors::UNKNOWN_MEMBER_ID => {
                    self.member_id.clear();
                    continue;
                }
                code => return Err(kafka_error(code, &format!("SyncGroup {}", group))),
            }
            self.membership = Some(Membership { coordinator, generation: resp.generation_id });
            let assigned = decode_assignment(&sresp.assignment)?;
            let mut out = Vec::new();
            for (t, parts) in assigned {
                for p in parts {
                    out.push((t.clone(), p));
                }
            }
            out.sort();
            return Ok(Some(out));
        }
        Ok(None)
    }

    fn heartbeat(&mut self) -> Result<()> {
        let (Some(m), Some(group)) = (&self.membership, &self.cfg.group_id) else { return Ok(()) };
        let req = HeartbeatRequest { group_id: group.clone(), generation_id: m.generation, member_id: self.member_id.clone() };
        let coordinator = m.coordinator;
        let code = self.client.heartbeat(coordinator, &req)?;
        self.last_heartbeat = Instant::now();
        match code {
            errors::NONE => {}
            errors::REBALANCE_IN_PROGRESS => {
                // commit what we processed before giving up the partitions
                self.flush_manual();
                if self.cfg.auto_commit_enable {
                    let _ = self.commit();
                }
                self.needs_assignment = true;
            }
            errors::UNKNOWN_MEMBER_ID => {
                self.member_id.clear();
                self.needs_assignment = true;
            }
            errors::ILLEGAL_GENERATION => self.needs_assignment = true,
            code => {
                self.membership = None;
                self.needs_assignment = true;
                cameleer_core::camel_debug!(&self.logger, "Heartbeat failed: {}", errors::name(code));
            }
        }
        Ok(())
    }

    // -----------------------------------------------------------------------
    // commits
    // -----------------------------------------------------------------------

    fn commit_offsets(&mut self, offsets: &HashMap<Tp, i64>) -> Result<()> {
        let (Some(m), Some(group)) = (&self.membership, &self.cfg.group_id) else { return Ok(()) };
        if offsets.is_empty() {
            return Ok(());
        }
        let mut by_topic: BTreeMap<String, Vec<(i32, i64, Option<String>)>> = BTreeMap::new();
        for ((t, p), off) in offsets {
            by_topic.entry(t.clone()).or_default().push((*p, *off, None));
        }
        let req = OffsetCommitRequest {
            group_id: group.clone(),
            generation_id: m.generation,
            member_id: self.member_id.clone(),
            retention_time_ms: -1,
            topics: by_topic.into_iter().collect(),
        };
        let coordinator = m.coordinator;
        let code = self.client.offset_commit(coordinator, &req)?;
        match code {
            errors::NONE => Ok(()),
            errors::REBALANCE_IN_PROGRESS | errors::ILLEGAL_GENERATION | errors::UNKNOWN_MEMBER_ID => {
                if code == errors::UNKNOWN_MEMBER_ID {
                    self.member_id.clear();
                }
                self.needs_assignment = true;
                Ok(())
            }
            code => Err(kafka_error(code, "OffsetCommit")),
        }
    }

    fn commit(&mut self) -> Result<()> {
        self.last_commit = Instant::now();
        if self.uncommitted.is_empty() {
            return Ok(());
        }
        let offsets = std::mem::take(&mut self.uncommitted);
        self.commit_offsets(&offsets)
    }

    fn flush_manual(&mut self) {
        let pending: HashMap<Tp, i64> = std::mem::take(&mut *self.manual.lock().unwrap());
        if !pending.is_empty() {
            if let Err(e) = self.commit_offsets(&pending) {
                cameleer_core::camel_warn!(&self.logger, "Manual commit failed: {}", e.message);
            }
        }
    }

    // -----------------------------------------------------------------------
    // polling
    // -----------------------------------------------------------------------

    fn poll(&mut self, flag: &StopFlag) -> Result<()> {
        // group partitions by leader
        let mut by_leader: BTreeMap<i32, Vec<(Tp, i64)>> = BTreeMap::new();
        let topics: Vec<String> = {
            let mut t: Vec<String> = self.assignment.iter().map(|(t, _)| t.clone()).collect();
            t.dedup();
            t
        };
        let mut leaders: HashMap<String, HashMap<i32, i32>> = HashMap::new();
        for t in topics {
            let l = self.client.leaders(&t)?;
            leaders.insert(t, l);
        }
        for tp in &self.assignment {
            let pos = *self.positions.get(tp).unwrap_or(&0);
            let leader = leaders.get(&tp.0).and_then(|l| l.get(&tp.1)).copied().unwrap_or(-1);
            by_leader.entry(leader).or_default().push((tp.clone(), pos));
        }
        let mut batch: Vec<(Tp, Record)> = Vec::new();
        for (leader, parts) in by_leader {
            let mut topics: BTreeMap<String, Vec<(i32, i64, i32)>> = BTreeMap::new();
            for ((t, p), pos) in &parts {
                topics.entry(t.clone()).or_default().push((*p, *pos, self.cfg.max_partition_fetch_bytes));
            }
            let req = FetchRequest {
                replica_id: -1,
                max_wait_ms: self.fetch_wait().as_millis() as i32,
                min_bytes: 1,
                max_bytes: self.cfg.fetch_max_bytes,
                isolation_level: 0,
                topics: topics.into_iter().collect(),
            };
            let resp = self.client.fetch(leader, &req)?;
            for (t, ps) in resp.topics {
                for p in ps {
                    let tp = (t.clone(), p.partition);
                    match p.error_code {
                        errors::NONE => {}
                        errors::OFFSET_OUT_OF_RANGE => {
                            let ts = if self.cfg.auto_offset_reset == OffsetReset::Latest { LATEST_TIMESTAMP } else { EARLIEST_TIMESTAMP };
                            if let Some(off) = self.client.list_offsets(&t, &[p.partition], ts)?.get(&p.partition) {
                                cameleer_core::camel_warn!(&self.logger, "Offset out of range for {}-{}, resetting to {}", t, p.partition, off);
                                self.positions.insert(tp, *off);
                            }
                            continue;
                        }
                        code if errors::is_retriable(code) => {
                            self.client.invalidate(&t);
                            continue;
                        }
                        code => return Err(kafka_error(code, &format!("Fetch {}-{}", t, p.partition))),
                    }
                    let pos = *self.positions.get(&tp).unwrap_or(&0);
                    for r in decode_record_batches(&p.records)? {
                        if r.offset >= pos {
                            batch.push((tp.clone(), r));
                        }
                    }
                }
            }
        }
        batch.truncate(self.cfg.max_poll_records);
        let n = batch.len();
        for (i, (tp, rec)) in batch.into_iter().enumerate() {
            if flag.is_stopped() {
                break;
            }
            let offset = rec.offset;
            let mut ex = self.exchange(&tp, rec, i + 1 == n);
            let ok = process_consumer_exchange(&self.logger, self.processor.as_ref(), &mut ex);
            if !ok && self.cfg.break_on_first_error {
                cameleer_core::camel_warn!(
                    &self.logger,
                    "Error processing record {}-{} offset {} with breakOnFirstError=true: the record will be consumed again",
                    tp.0,
                    tp.1,
                    offset
                );
                if self.cfg.auto_commit_enable {
                    self.commit()?;
                }
                flag.sleep(self.fetch_wait());
                return Ok(());
            }
            self.positions.insert(tp.clone(), offset + 1);
            if self.cfg.auto_commit_enable {
                self.uncommitted.insert(tp, offset + 1);
            }
            self.flush_manual();
        }
        Ok(())
    }

    fn exchange(&self, tp: &Tp, rec: Record, last: bool) -> Exchange {
        let mut ex = Exchange::new(self.ctx.upgrade());
        let body = match rec.value {
            None => Value::Null,
            Some(v) if self.cfg.value_as_bytes => Value::Bytes(v),
            Some(v) => Value::String(String::from_utf8_lossy(&v).into_owned()),
        };
        ex.set_body(body);
        ex.set_header(headers::TOPIC, tp.0.clone());
        ex.set_header(headers::PARTITION, tp.1 as i64);
        ex.set_header(headers::OFFSET, rec.offset);
        ex.set_header(headers::TIMESTAMP, rec.timestamp);
        if let Some(k) = rec.key {
            ex.set_header(headers::KEY, if self.cfg.key_as_bytes { Value::Bytes(k) } else { Value::String(String::from_utf8_lossy(&k).into_owned()) });
        }
        ex.set_header(headers::LAST_POLL_RECORD, last);
        if !self.cfg.auto_commit_enable {
            ex.set_header(headers::LAST_RECORD_BEFORE_COMMIT, last);
        }
        for (k, v) in rec.headers {
            if !is_filtered_header(&k) {
                ex.set_header(k, v.map(Value::Bytes).unwrap_or(Value::Null));
            }
        }
        if self.cfg.allow_manual_commit {
            ex.set_header(
                headers::MANUAL_COMMIT,
                Value::object([
                    ("consumerId", Value::String(self.consumer_id.clone())),
                    ("topic", Value::String(tp.0.clone())),
                    ("partition", Value::Int(tp.1 as i64)),
                    ("offset", Value::Int(rec.offset)),
                ]),
            );
        }
        ex
    }
}
