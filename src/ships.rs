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
    #[allow(dead_code)] // no Grand-class hull in the ship table yet
    Grand,
}

// Reference data mirroring the yppedia hull tables; several columns aren't read
// yet, so allow unused fields across the struct.
#[allow(dead_code)]
pub struct Ship {
    pub name: &'static str,
    /// Short two-character hull tag for compact tables (e.g. the Ship Winrate
    /// matrix headers). Community-standard abbreviations; `[char; 2]` so the
    /// length is enforced at compile time.
    pub abbr: [char; 2],
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
    /// Ram size class. Retained as reference data (and to document each hull's
    /// collision tier); the calculator no longer auto-doubles head-ons — the
    /// user enters a different-class head-on as two rams instead.
    pub ship_size_class: ShipClass,
    pub move_tokens: u8,
    /// Maximum *mercenaries* (improved NPC crew) hireable, per yppedia. Used
    /// as the swabbie cap for staffing checks — mercenaries, not basic
    /// swabbies.
    pub max_mercenaries: u8,
    /// Maximum pirates that can be aboard (Capacity "Pirates" column).
    pub max_pirates: u8,
}

const SLOOP: Ship = Ship {
    name: "Sloop",
    abbr: ['S', 'l'],
    max_mercenaries: 6,
    max_pirates: 7,
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
    abbr: ['C', 't'],
    max_mercenaries: 10,
    max_pirates: 12,
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
    abbr: ['D', 'h'],
    max_mercenaries: 10,
    max_pirates: 12,
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
    abbr: ['F', 'n'],
    max_mercenaries: 10,
    max_pirates: 12,
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
    abbr: ['L', 'S'],
    max_mercenaries: 13,
    max_pirates: 15,
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
    abbr: ['B', 'g'],
    max_mercenaries: 16,
    max_pirates: 18,
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
    abbr: ['J', 'n'],
    max_mercenaries: 16,
    max_pirates: 18,
    sail_stations: 6,
    bilge_stations: 4,
    carpentry_stations: 4,
    gun_sets: 3,
    cannon_size: CannonSize::Large,
    shots_per_move: 1,
    mass: 18000,
    volume: 27000,
    morale_hp: 14400,
    hull_hp: 24000,
    ram_damage: 1440,
    ship_size_class: ShipClass::Medium,
    move_tokens: 3,
};

const MERCHANT_BRIG: Ship = Ship {
    name: "Merchant Brig",
    abbr: ['M', 'B'],
    max_mercenaries: 18,
    max_pirates: 20,
    sail_stations: 6,
    bilge_stations: 6,
    carpentry_stations: 9,
    gun_sets: 2,
    cannon_size: CannonSize::Medium,
    shots_per_move: 1,
    mass: 90000,
    volume: 135000,
    morale_hp: 11520,
    hull_hp: 19200,
    ram_damage: 960,
    ship_size_class: ShipClass::Medium,
    move_tokens: 3,
};

const WAR_BRIG: Ship = Ship {
    name: "War Brig",
    abbr: ['W', 'B'],
    max_mercenaries: 23,
    max_pirates: 30,
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
    abbr: ['M', 'G'],
    max_mercenaries: 28,
    max_pirates: 30,
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
    abbr: ['W', 'G'],
    max_mercenaries: 32,
    max_pirates: 40,
    sail_stations: 12,
    bilge_stations: 7,
    carpentry_stations: 8,
    gun_sets: 6,
    cannon_size: CannonSize::Large,
    shots_per_move: 2,
    mass: 90000,
    volume: 135000,
    morale_hp: 14400,
    hull_hp: 24000,
    ram_damage: 2400, // NOTE: unknown
    ship_size_class: ShipClass::Large,
    move_tokens: 3,
};

const XEBEC: Ship = Ship {
    name: "Xebec",
    abbr: ['X', 'b'],
    max_mercenaries: 36,
    max_pirates: 45,
    sail_stations: 14,
    bilge_stations: 8,
    carpentry_stations: 9,
    gun_sets: 6,
    cannon_size: CannonSize::Medium,
    shots_per_move: 2,
    mass: 121500,
    volume: 182250,
    morale_hp: 20160,
    hull_hp: 33600,
    ram_damage: 2400,
    ship_size_class: ShipClass::Large,
    move_tokens: 3,
};

const WAR_FRIGATE: Ship = Ship {
    name: "War Frigate",
    abbr: ['W', 'F'],
    max_mercenaries: 54,
    max_pirates: 75,
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
    abbr: ['G', 'F'],
    max_mercenaries: 75,
    max_pirates: 159,
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

pub const SHIPS: &[Ship] = &[
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

/// Index into [`SHIPS`] of the ship with this exact display name, if any. Used
/// to resolve a known foe hull (special encounters) to a calculator ship index.
pub fn ship_index(name: &str) -> Option<usize> {
    SHIPS.iter().position(|s| s.name == name)
}

/// Index into [`SHIPS`] of the smallest-capacity hull that can carry `crew`
/// pirates — the fitting hull with the fewest [`Ship::max_pirates`]. `None`
/// when no hull is large enough (a crew beyond the Grand Frigate's capacity).
/// Used to suggest a foe hull when an observed headcount exceeds the selected
/// ship's pirate capacity.
pub fn smallest_ship_for(crew: u32) -> Option<usize> {
    SHIPS
        .iter()
        .enumerate()
        .filter(|(_, s)| s.max_pirates as u32 >= crew)
        .min_by_key(|(_, s)| s.max_pirates)
        .map(|(i, _)| i)
}
