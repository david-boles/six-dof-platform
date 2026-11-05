use bitfield_struct::{bitenum, bitfield};
use endian_num::Le;
use esp_hal::{
    Blocking,
    i2c::master::{Error, I2c},
};

#[derive(Debug, PartialEq, Eq)]
#[repr(u8)]
#[bitenum]
pub enum Mux {
    Ain0Ain1 = 0b0000,
    Ain0Ain2 = 0b0001,
    Ain0Ain3 = 0b0010,
    Ain1Ain0 = 0b0011,
    Ain1Ain2 = 0b0100,
    Ain1Ain3 = 0b0101,
    Ain2Ain3 = 0b0110,
    Ain3Ain2 = 0b0111,
    Ain0Avss = 0b1000,
    Ain1Avss = 0b1001,
    Ain2Avss = 0b1010,
    Ain3Avss = 0b1011,
    VrefpVrefnDiv4PgaBypass = 0b1100,
    AvddAvssDiv4PgaBypass = 0b1101,
    ShortToAvddAvssDiv2 = 0b1110,
    Reserved = 0b1111,
    #[fallback]
    Invalid = 0xff,
}

#[derive(Debug, PartialEq, Eq)]
#[repr(u8)]
#[bitenum]
pub enum Gain {
    G1 = 0b000,
    G2 = 0b001,
    G4 = 0b010,
    G8 = 0b011,
    G16 = 0b100,
    G32 = 0b101,
    G64 = 0b110,
    G128 = 0b111,
    #[fallback]
    Invalid = 0xff,
}

#[bitfield(u8)]
pub struct Config0 {
    #[bits(1)]
    pub pga_bypass: bool,
    #[bits(3)]
    pub gain: Gain,
    #[bits(4)]
    pub mux: Mux,
}

#[derive(Debug, PartialEq, Eq)]
#[repr(u8)]
#[bitenum]
pub enum DataRate {
    Normal20Turbo40 = 0b000,
    Normal45Turbo90 = 0b001,
    Normal90Turbo180 = 0b010,
    Normal175Turbo350 = 0b011,
    Normal330Turbo660 = 0b100,
    Normal600Turbo1200 = 0b101,
    Normal1000Turbo2000 = 0b110,
    Reserved = 0b111,
    #[fallback]
    Invalid = 0xff,
}

#[derive(Debug, PartialEq, Eq)]
#[repr(u8)]
#[bitenum]
pub enum Vref {
    Internal = 0b00,
    External = 0b01,
    AnalogSupply = 0b10,
    AnalogSupply2 = 0b11,
    #[fallback]
    Invalid = 0xff,
}

#[bitfield(u8)]
pub struct Config1 {
    #[bits(1)]
    pub temperature_mode: bool,
    #[bits(2)]
    pub vref: Vref,
    #[bits(1)]
    pub continuous_mode: bool,
    #[bits(1)]
    pub turbo_mode: bool,
    #[bits(3)]
    pub data_rate: DataRate,
}

#[derive(Debug, PartialEq, Eq)]
#[repr(u8)]
#[bitenum]
pub enum DataIntegrity {
    Disabled = 0b00,
    Inverted = 0b01,
    CRC16 = 0b10,
    Reserved = 0b11,
    #[fallback]
    Invalid = 0xff,
}

#[derive(Debug, PartialEq, Eq)]
#[repr(u8)]
#[bitenum]
pub enum IdacCurrent {
    Off = 0b000,
    I10 = 0b001,
    I50 = 0b010,
    I100 = 0b011,
    I250 = 0b100,
    I500 = 0b101,
    I1000 = 0b110,
    I1500 = 0b111,
    #[fallback]
    Invalid = 0xff,
}

#[bitfield(u8)]
pub struct Config2 {
    #[bits(3)]
    pub idac_current: IdacCurrent,
    #[bits(1)]
    pub burn_out_current_sources_en: bool,
    #[bits(2)]
    pub data_integrity: DataIntegrity,
    #[bits(1)]
    pub data_counter_en: bool,
    #[bits(1)]
    pub conversion_ready: bool,
}

#[derive(Debug, PartialEq, Eq)]
#[repr(u8)]
#[bitenum]
pub enum IdacMux {
    Disabled = 0b000,
    Ain0 = 0b001,
    Ain1 = 0b010,
    Ain2 = 0b011,
    Ain3 = 0b100,
    Refp = 0b101,
    Refn = 0b110,
    Reserved = 0b111,
    #[fallback]
    Invalid = 0xff,
}

#[bitfield(u8)]
pub struct Config3 {
    #[bits(2)]
    __: u8,
    #[bits(3)]
    pub idac2_mux: IdacMux,
    #[bits(3)]
    pub idac1_mux: IdacMux,
}

#[bitfield(u32, repr = Le::<u32>, from = Le::<u32>::from_ne, into = Le::<u32>::to_ne)]
pub struct Config {
    #[bits(8)]
    pub config3: Config3,
    #[bits(8)]
    pub config2: Config2,
    #[bits(8)]
    pub config1: Config1,
    #[bits(8)]
    pub config0: Config0,
}

impl Config {
    pub fn read(i2c: &mut I2c<'_, Blocking>, addr: u8) -> Result<Config, Error> {
        // XXX: Reads seem to need to happen in separate transactions (e.g. re-send the address, which _is_ implicated by the datasheet).
        let c0 = Config0::read(i2c, addr)?;
        let c1 = Config1::read(i2c, addr)?;
        let c2 = Config2::read(i2c, addr)?;
        let c3 = Config3::read(i2c, addr)?;
        Ok(Config::new()
            .with_config0(c0)
            .with_config1(c1)
            .with_config2(c2)
            .with_config3(c3))
    }

    pub fn write(&self, i2c: &mut I2c<'_, Blocking>, addr: u8) -> Result<(), Error> {
        #[allow(clippy::identity_op)]
        let ops = [
            0b0100_0000 | (0 << 2),
            self.config0().into(),
            0b0100_0000 | (1 << 2),
            self.config1().into(),
            0b0100_0000 | (2 << 2),
            self.config2().into(),
            0b0100_0000 | (3 << 2),
            self.config3().into(),
        ];

        i2c.write(addr, &ops)
    }
}

pub trait Register: From<u8> + Into<u8> + Copy {
    const ADDR: u8;

    fn read(i2c: &mut I2c<'_, Blocking>, addr: u8) -> Result<Self, Error> {
        let mut buf = [0; 1];
        i2c.write_read(addr, &[0b0010_0000 | (Self::ADDR << 2)], &mut buf)
            .map(|_| Self::from(buf[0]))
    }

    fn write(&self, i2c: &mut I2c<'_, Blocking>, addr: u8) -> Result<(), Error> {
        let buf = [0b0100_0000 | (Self::ADDR << 2), (*self).into()];
        i2c.write(addr, &buf)
    }
}
impl Register for Config0 {
    const ADDR: u8 = 0;
}
impl Register for Config1 {
    const ADDR: u8 = 1;
}
impl Register for Config2 {
    const ADDR: u8 = 2;
}
impl Register for Config3 {
    const ADDR: u8 = 3;
}

pub fn reset(i2c: &mut I2c<'_, Blocking>, addr: u8) -> Result<(), Error> {
    i2c.write(addr, &[0b0000_0110])
}

pub fn start(i2c: &mut I2c<'_, Blocking>, addr: u8) -> Result<(), Error> {
    i2c.write(addr, &[0b0000_1000])
}

pub fn powerdown(i2c: &mut I2c<'_, Blocking>, addr: u8) -> Result<(), Error> {
    i2c.write(addr, &[0b0000_0010])
}

pub fn read_data(i2c: &mut I2c<'_, Blocking>, addr: u8, buf: &mut [u8]) -> Result<(), Error> {
    i2c.write_read(addr, &[0b0001_0000], buf)
}

/// `buf.len() % 2 == 0`.
#[allow(clippy::result_unit_err)]
pub fn validate_inversion(buf: &[u8]) -> Result<(), ()> {
    let (orig, inv) = buf.split_at(buf.len() / 2);
    orig.iter().zip(inv.iter()).all(|(o, i)| *o == !i).ok_or(())
}
