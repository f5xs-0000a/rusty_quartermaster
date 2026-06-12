#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CannonSize {
    Small,
    Medium,
    Large,
}

impl CannonSize {
    pub fn damage(self) -> u32 {
        match self {
            CannonSize::Small => 960,
            CannonSize::Medium => 1440,
            CannonSize::Large => 1920,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShipClass {
    Small,
    Medium,
    Large,
    Grand,
}

pub struct Ship {
    pub name: &'static str,
    pub sail_stations: u8,
    pub bilge_stations: u8,
    pub carpentry_stations: u8,
    pub gun_sets: u8,
    pub cannon_size: CannonSize,
    pub shots_per_move: u8,
    pub mass: u32,
    pub volume: u32,
    pub hull_hp: u32,
    pub morale_hp: u32,
    pub ram_damage: u32,
    pub ship_size_class: ShipClass,
    pub move_tokens: u8,
}

const SLOOP: Ship = Ship {
    name: "Sloop",
    sail_stations: 3,
    bilge_stations: 2,
    carpentry_stations: 2,
    gun_sets: 1,
    cannon_size: CannonSize::Small,
    shots_per_move: 1,
    mass: 13500,
    volume: 20250,
    morale_hp: 5760,
    hull_hp: 9600,
    ram_damage: 480,
    ship_size_class: ShipClass::Small,
    move_tokens: 4,
};

const CUTTER: Ship = Ship {
    name: "Cutter",
    sail_stations: 5,
    bilge_stations: 2,
    carpentry_stations: 3,
    gun_sets: 2,
    cannon_size: CannonSize::Small,
    shots_per_move: 1,
    mass: 40500,
    volume: 60750,
    morale_hp: 7200,
    hull_hp: 11520,
    ram_damage: 480,
    ship_size_class: ShipClass::Small,
    move_tokens: 4,
};

const DHOW: Ship = Ship {
    name: "Dhow",
    sail_stations: 5,
    bilge_stations: 2,
    carpentry_stations: 3,
    gun_sets: 1,
    cannon_size: CannonSize::Medium,
    shots_per_move: 1,
    mass: 13500,
    volume: 20250,
    morale_hp: 7200,
    hull_hp: 11520,
    ram_damage: 480,
    ship_size_class: ShipClass::Small,
    move_tokens: 4,
};

const FANCHUAN: Ship = Ship {
    name: "Fanchuan",
    sail_stations: 5,
    bilge_stations: 2,
    carpentry_stations: 3,
    gun_sets: 1,
    cannon_size: CannonSize::Large,
    shots_per_move: 1,
    mass: 13500,
    volume: 20250,
    morale_hp: 7560,
    hull_hp: 12600,
    ram_damage: 480,
    ship_size_class: ShipClass::Small,
    move_tokens: 3,
};

const LONGSHIP: Ship = Ship {
    name: "Longship",
    sail_stations: 5,
    bilge_stations: 3,
    carpentry_stations: 3,
    gun_sets: 3,
    cannon_size: CannonSize::Small,
    shots_per_move: 2,
    mass: 13500,
    volume: 20250,
    morale_hp: 8640,
    hull_hp: 14400,
    ram_damage: 480,
    ship_size_class: ShipClass::Medium,
    move_tokens: 4,
};

const BAGHLAH: Ship = Ship {
    name: "Baghlah",
    sail_stations: 6,
    bilge_stations: 4,
    carpentry_stations: 4,
    gun_sets: 3,
    cannon_size: CannonSize::Medium,
    shots_per_move: 2,
    mass: 18000,
    volume: 27000,
    morale_hp: 11520,
    hull_hp: 19200,
    ram_damage: 960,
    ship_size_class: ShipClass::Medium,
    move_tokens: 3,
};

const JUNK: Ship = Ship {
    name: "Junk",
    sail_stations: 6,
    bilge_stations: 4,
    carpentry_stations: 4,
    gun_sets: 3,
    cannon_size: CannonSize::Large,
    shots_per_move: 1,
    mass: 18000,
    volume: 27000,
    morale_hp: 11520,
    hull_hp: 19200,
    ram_damage: 960,
    ship_size_class: ShipClass::Medium,
    move_tokens: 3,
};

const MERCHANT_BRIG: Ship = Ship {
    name: "Merchant Brig",
    sail_stations: 6,
    bilge_stations: 6,
    carpentry_stations: 9,
    gun_sets: 2,
    cannon_size: CannonSize::Medium,
    shots_per_move: 1,
    mass: 90000,
    volume: 135000,
    morale_hp: 14400,
    hull_hp: 24000,
    ram_damage: 1440,
    ship_size_class: ShipClass::Medium,
    move_tokens: 3,
};

const WAR_BRIG: Ship = Ship {
    name: "War Brig",
    sail_stations: 9,
    bilge_stations: 4,
    carpentry_stations: 6,
    gun_sets: 4,
    cannon_size: CannonSize::Medium,
    shots_per_move: 2,
    mass: 54000,
    volume: 81000,
    morale_hp: 14400,
    hull_hp: 24000,
    ram_damage: 1920,
    ship_size_class: ShipClass::Medium,
    move_tokens: 3,
};

const MERCHANT_GALLEON: Ship = Ship {
    name: "Merchant Galleon",
    sail_stations: 9,
    bilge_stations: 14,
    carpentry_stations: 14,
    gun_sets: 3,
    cannon_size: CannonSize::Large,
    shots_per_move: 1,
    mass: 270000,
    volume: 405000,
    morale_hp: 17280,
    hull_hp: 28800,
    ram_damage: 2400,
    ship_size_class: ShipClass::Large,
    move_tokens: 3,
};

const WAR_GALLEON: Ship = Ship {
    name: "War Galleon",
    sail_stations: 12,
    bilge_stations: 7,
    carpentry_stations: 8,
    gun_sets: 6,
    cannon_size: CannonSize::Large,
    shots_per_move: 2,
    mass: 90000,
    volume: 135000,
    morale_hp: 20160,
    hull_hp: 33600,
    ram_damage: 2400, // NOTE: unknown
    ship_size_class: ShipClass::Large,
    move_tokens: 3,
};

const XEBEC: Ship = Ship {
    name: "Xebec",
    sail_stations: 14,
    bilge_stations: 8,
    carpentry_stations: 9,
    gun_sets: 6,
    cannon_size: CannonSize::Medium,
    shots_per_move: 2,
    mass: 121500,
    volume: 182250,
    morale_hp: 14400,
    hull_hp: 24000,
    ram_damage: 2400,
    ship_size_class: ShipClass::Large,
    move_tokens: 3,
};

const WAR_FRIGATE: Ship = Ship {
    name: "War Frigate",
    sail_stations: 18,
    bilge_stations: 12,
    carpentry_stations: 18,
    gun_sets: 6,
    cannon_size: CannonSize::Large,
    shots_per_move: 2,
    mass: 216000,
    volume: 324000,
    morale_hp: 28800,
    hull_hp: 48000,
    ram_damage: 2880,
    ship_size_class: ShipClass::Large,
    move_tokens: 3,
};

const GRAND_FRIGATE: Ship = Ship {
    name: "Grand Frigate",
    sail_stations: 30,
    bilge_stations: 16,
    carpentry_stations: 24,
    gun_sets: 6,
    cannon_size: CannonSize::Large,
    shots_per_move: 2,
    mass: 540000,
    volume: 810000,
    morale_hp: 34560,
    hull_hp: 57600,
    ram_damage: 3840,
    ship_size_class: ShipClass::Large,
    move_tokens: 3,
};

pub static SHIPS: &[Ship] = &[
    SLOOP,
    CUTTER,
    DHOW,
    FANCHUAN,
    LONGSHIP,
    BAGHLAH,
    JUNK,
    MERCHANT_BRIG,
    WAR_BRIG,
    MERCHANT_GALLEON,
    WAR_GALLEON,
    XEBEC,
    WAR_FRIGATE,
    GRAND_FRIGATE,
];
