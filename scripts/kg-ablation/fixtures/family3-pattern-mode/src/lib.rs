//! Fixture pack for ablation task family 3 (call-shape pattern queries).
//! The method name `reset` exists on two traits (`Node`, `Leaf`) and as
//! a free function, so a text query for `reset` cannot separate the
//! call sites by receiver type.

pub trait Node {
    fn reset(&mut self);
    fn generation(&self) -> u32;
}

pub trait Leaf {
    fn reset(&mut self);
    fn label(&self) -> String;
}

pub struct Sensor {
    pub ticks: u32,
}

pub struct Register {
    pub word: u32,
}

pub struct CacheLine {
    pub tag: String,
}

impl Node for Sensor {
    fn reset(&mut self) {
        self.ticks = 0;
    }

    fn generation(&self) -> u32 {
        self.ticks
    }
}

impl Node for Register {
    fn reset(&mut self) {
        self.word = 0;
    }

    fn generation(&self) -> u32 {
        self.word
    }
}

impl Leaf for CacheLine {
    fn reset(&mut self) {
        self.tag = String::new();
    }

    fn label(&self) -> String {
        self.tag.clone()
    }
}

pub fn reset(word: u32) -> u32 {
    0
}

pub fn cycle(sensor: &mut Sensor, register: &mut Register,
             line: &mut CacheLine, seed: u32) -> u32 {
    let before = sensor.generation() + register.generation();
    sensor.reset();
    register.reset();
    line.reset();
    let after = sensor.generation() + register.generation();
    before + after + reset(seed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cycle_returns_seed() {
        let mut s = Sensor { ticks: 3 };
        let mut r = Register { word: 4 };
        let mut c = CacheLine { tag: String::from("t") };
        assert_eq!(cycle(&mut s, &mut r, &mut c, 9), 7);
    }

    #[test]
    fn reset_free_function_returns_zero() {
        assert_eq!(reset(7), 0);
    }
}
