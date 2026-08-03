pub const MEMORY: u64 = 32 * 1024 * 1024;

pub fn digest(bytes: &[u8]) -> u64 {
    let mut state = bytes.len() as u64;
    let mut chunks = bytes.chunks_exact(8);
    for chunk in &mut chunks {
        state = state.rotate_left(7) ^ u64::from_le_bytes(chunk.try_into().unwrap());
    }
    for &byte in chunks.remainder() {
        state = state.rotate_left(7) ^ u64::from(byte);
    }
    state
}

pub fn payload(seed: u64, _operation: u64, len: usize) -> Vec<u8> {
    (0..len).map(|index| payload_byte(seed, index)).collect()
}

fn payload_byte(seed: u64, index: usize) -> u8 {
    seed.wrapping_add((index as u64).wrapping_mul(0x9e37_79b9))
        .to_le_bytes()[index & 7]
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub enum Status {
    Ok,
    Unsupported,
    Invalid,
    SetupError,
    TimedError,
    DrainError,
}

#[derive(Clone, Debug, Hash, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub struct Cell {
    pub arm: String,
    pub payload: u64,
    pub capacity: u64,
    pub in_flight: u64,
    pub memory: u64,
}

impl Cell {
    pub fn condition(&self) -> Condition {
        Condition {
            payload: self.payload,
            capacity: self.capacity,
            in_flight: self.in_flight,
            memory: self.memory,
        }
    }
}

#[derive(
    Clone, Copy, Debug, Hash, PartialEq, Eq, PartialOrd, Ord, serde::Deserialize, serde::Serialize,
)]
pub struct Condition {
    pub payload: u64,
    pub capacity: u64,
    pub in_flight: u64,
    pub memory: u64,
}

impl Condition {
    pub fn cell(self, arm: &super::family::Arm) -> Cell {
        Cell {
            arm: arm.key.into(),
            payload: self.payload,
            capacity: self.capacity,
            in_flight: self.in_flight,
            memory: self.memory,
        }
    }
}

pub fn condition(payload: u64, capacity: u64, in_flight: u64) -> Condition {
    Condition {
        payload,
        capacity,
        in_flight,
        memory: MEMORY,
    }
}

#[derive(Clone, Copy)]
pub struct Scheduled {
    pub block: u32,
    pub order: u32,
    pub condition: Condition,
    pub arm: &'static super::family::Arm,
}

impl Scheduled {
    pub fn cell(self) -> Cell {
        self.condition.cell(self.arm)
    }
}

pub fn schedule(
    members: &[(Condition, &'static super::family::Arm)],
    blocks: u32,
    seed: u64,
) -> Vec<Scheduled> {
    let mut state = seed;
    let mut result = Vec::with_capacity(members.len() * blocks as usize);
    for block in 0..blocks {
        let mut shuffled = members.to_vec();
        for index in (1..shuffled.len()).rev() {
            let selected = super::study::random(&mut state) as usize % (index + 1);
            shuffled.swap(index, selected);
        }
        for (order, (condition, arm)) in shuffled.into_iter().enumerate() {
            result.push(Scheduled {
                block,
                order: order as u32,
                condition,
                arm,
            });
        }
    }
    result
}

pub fn window(remaining: u64, capacity: u64, in_flight: u64) -> u64 {
    remaining.min(capacity).min(in_flight)
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub struct Observed {
    pub payload: u64,
    pub capacity: u64,
    pub in_flight: u64,
    pub window: u64,
    pub topology: String,
    pub transport: String,
    pub extent: Option<u64>,
    pub allocator: Option<String>,
    pub socket_send: Option<u64>,
    pub socket_recv: Option<u64>,
}
