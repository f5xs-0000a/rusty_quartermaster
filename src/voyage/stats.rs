//! Voyage consumption statistics derived from the profits inventory and the
//! timed voyage.
//!
//! Consumption is not in the chat log — it's a **stock delta**. We assume a run
//! starts with the Restock amount of each commodity and ends with the Stock
//! amount, so whatever's missing was used (`Restock - Stock`). Booty is goods
//! *won* and isn't consumable, so it's ignored here. See the
//! `voyage-statistics-model` memory and [`crate::voyage::Voyage`].

use crate::api::Commodity;
use crate::profits::InventoryRow;
use crate::ships::CannonSize;
use crate::voyage::{BattleCategory, BattleOutcome, Voyage};

/// Caveat to show beside the rum-spice figures: spice consumption can't be read
/// accurately from a stock delta when swabbies are aboard (they share the pool)
/// or when spice ran out partway through the run.
pub const RUM_SPICE_CAVEAT: &str =
    "Not an accurate representation of rum spice, especially if you run with \
     swabbies or ran out of rum spice mid-voyage.";

/// Consumption over a voyage plus the per-crew / per-minute rates derived from
/// it. Rate fields are `None` when their denominator is unavailable: no ship
/// picked (balls), no battles yet (per-battle), or the run hasn't ported so
/// there's no duration / time-weighted average crew (the per-minute / per-crew
/// figures).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
#[allow(dead_code)] // consumed by the Voyage Statistics UI (task #7)
pub struct ConsumptionStats {
    /// Cannonballs of the ship's cannon size used (`Restock - Stock`). `None`
    /// when the ship — hence cannon size — isn't known.
    pub balls: Option<u64>,
    /// Average balls per battle this voyage.
    pub balls_per_battle: Option<f64>,
    /// Alcohol used, in potency-weighted units (Swill×2 + Grog×3 + Fine rum×6),
    /// matching the Hold Stats "alcohol" figure.
    pub alcohol: u64,
    /// Alcohol per (pirate + swabbie), over the time-weighted average crew.
    pub alcohol_per_crew: Option<f64>,
    /// Alcohol per (pirate + swabbie) per minute.
    pub alcohol_per_crew_per_min: Option<f64>,
    /// Rum spice used (`Restock - Stock`). See [`RUM_SPICE_CAVEAT`].
    pub rum_spice: u64,
    /// Rum spice per swabbie (time-weighted average swabbies).
    pub rum_spice_per_swabbie: Option<f64>,
    /// Rum spice per swabbie per minute.
    pub rum_spice_per_swabbie_per_min: Option<f64>,
}

/// The cannonball commodity name matching a cannon size.
fn cannonball_name(size: CannonSize) -> &'static str {
    match size {
        CannonSize::Small => "Small cannon balls",
        CannonSize::Medium => "Medium cannon balls",
        CannonSize::Large => "Large cannon balls",
    }
}

/// Quantity consumed of one commodity by canonical name: `Restock - Stock`
/// summed over matching rows, saturating at zero (a stock *gain* isn't usage).
fn used_by_name(rows: &[InventoryRow], commodities: &[Commodity], name: &str) -> u64 {
    rows.iter()
        .filter(|r| crate::app::commod_name(commodities, r.commod_id).eq_ignore_ascii_case(name))
        .map(|r| {
            let restock = r.restock.parse::<u64>().unwrap_or(0);
            let stock = r.stock.parse::<u64>().unwrap_or(0);
            restock.saturating_sub(stock)
        })
        .sum()
}

/// Alcohol used = potency-weighted `Restock - Stock` summed over all rows. The
/// delta is taken per row *after* weighting so the three rum tiers are comparable
/// (this equals Hold Stats' restock-alcohol minus its ship-hold-alcohol).
fn alcohol_used(rows: &[InventoryRow], commodities: &[Commodity]) -> u64 {
    rows.iter()
        .map(|r| {
            let name = crate::app::commod_name(commodities, r.commod_id);
            let mult = crate::commodities::alcohol_multiplier(name);
            if mult == 0 {
                return 0;
            }
            let restock = r.restock.parse::<u64>().unwrap_or(0);
            let stock = r.stock.parse::<u64>().unwrap_or(0);
            restock.saturating_sub(stock) * mult
        })
        .sum()
}

/// Compute consumption stats for `voyage` from the current inventory `rows` and
/// the chosen ship's `cannon_size` (`None` if no ship is picked).
#[allow(dead_code)] // consumed by the Voyage Statistics UI (task #7)
pub fn consumption_stats(
    voyage: &Voyage,
    rows: &[InventoryRow],
    commodities: &[Commodity],
    cannon_size: Option<CannonSize>,
) -> ConsumptionStats {
    let balls = cannon_size.map(|sz| used_by_name(rows, commodities, cannonball_name(sz)));
    let alcohol = alcohol_used(rows, commodities);
    let rum_spice = used_by_name(rows, commodities, "Rum spice");

    let battles = voyage.battles.len() as u32;
    let minutes = voyage
        .duration_secs()
        .map(|s| s as f64 / 60.0)
        .filter(|m| *m > 0.0);
    let avg_swabbies = voyage.avg_swabbies();
    let avg_crew = match (voyage.avg_pirates(), avg_swabbies) {
        (Some(p), Some(s)) => Some(p + s),
        _ => None,
    };

    // Divide `amount` by a denominator that must be present and positive.
    let per = |amount: u64, denom: Option<f64>| {
        denom.filter(|d| *d > 0.0).map(|d| amount as f64 / d)
    };

    let balls_per_battle = balls
        .filter(|_| battles > 0)
        .map(|b| b as f64 / battles as f64);
    let alcohol_per_crew = per(alcohol, avg_crew);
    let alcohol_per_crew_per_min = alcohol_per_crew.and_then(|a| minutes.map(|m| a / m));
    let rum_spice_per_swabbie = per(rum_spice, avg_swabbies);
    let rum_spice_per_swabbie_per_min = rum_spice_per_swabbie.and_then(|a| minutes.map(|m| a / m));

    ConsumptionStats {
        balls,
        balls_per_battle,
        alcohol,
        alcohol_per_crew,
        alcohol_per_crew_per_min,
        rum_spice,
        rum_spice_per_swabbie,
        rum_spice_per_swabbie_per_min,
    }
}

/// Aggregate battle / loot / timing stats for a voyage. Per-crew figures use the
/// time-weighted average crew and are `None` until the run has ported (no
/// duration / average yet). Disengaged fights count in the winrate and battle
/// timing but not in the loot denominators.
#[derive(Clone, Debug, Default, PartialEq)]
#[allow(dead_code)] // consumed by the Voyage Statistics UI (task #7)
pub struct BattleStats {
    pub wins: u32,
    pub losses: u32,
    pub disengages: u32,
    /// Mean whole-engagement duration (intercept -> resolution), seconds.
    pub avg_battle_secs: Option<f64>,
    /// Population σ of the whole-engagement durations, seconds (None if n < 3).
    pub avg_battle_sd: Option<f64>,
    /// Mean naval-phase duration (intercept -> grapple), seconds. The UI shows
    /// this as turns (35s per turn).
    pub avg_naval_secs: Option<f64>,
    /// Population σ of the naval-phase durations, seconds (None if n < 3).
    pub avg_naval_sd: Option<f64>,
    /// Mean boarding-melee duration (grapple -> resolution), seconds.
    pub avg_boarding_secs: Option<f64>,
    /// Population σ of the boarding-melee durations, seconds (None if n < 3).
    pub avg_boarding_sd: Option<f64>,
    /// Total time spent in battle (sum of engagement durations), seconds.
    pub time_in_battle_secs: i64,
    /// Time at sea but not fighting = voyage duration − time in battle. `None`
    /// until ported.
    pub time_at_sea_secs: Option<i64>,
    /// Gross PoE plundered across won fights (>= 0).
    pub poe_won_total: i64,
    /// Net PoE across all fights (losses subtract).
    pub poe_net_total: i64,
    /// Units of goods won across won fights.
    pub goods_won_total: u64,
    /// Mean gross PoE per won fight.
    pub poe_per_fight_won: Option<f64>,
    /// Population σ of per-won-fight PoE (None if n < 3).
    pub poe_per_fight_won_sd: Option<f64>,
    /// Mean net PoE per decisive fight (wins + losses).
    pub poe_per_fight_net: Option<f64>,
    /// Population σ of per-decisive-fight net PoE (None if n < 3).
    pub poe_per_fight_net_sd: Option<f64>,
    /// Mean units of goods per won fight.
    pub goods_per_fight: Option<f64>,
    /// Population σ of per-won-fight goods (None if n < 3).
    pub goods_per_fight_sd: Option<f64>,
    /// Mean net units of goods per decisive fight (goods lost in defeats
    /// subtract). Can be negative.
    pub goods_per_engagement: Option<f64>,
    /// Population σ of per-decisive-fight net goods (None if n < 3).
    pub goods_per_engagement_sd: Option<f64>,
    /// Net PoE per crew member (time-weighted avg crew).
    pub poe_per_crew: Option<f64>,
    /// Gross-won PoE per crew per won fight.
    pub poe_per_crew_per_fight_won: Option<f64>,
    /// Net PoE per crew per decisive fight (wins + losses).
    pub poe_per_crew_per_fight_all: Option<f64>,
    /// Per-enemy-category outcome tallies (label, W/L/D), most-fought first.
    /// Only categories that actually occurred appear — zero categories omitted.
    pub categories: Vec<(String, CategoryTally)>,
    /// Mean damage advantage over fights where it was tracked (`[-0.5, 0.5]`).
    pub avg_advantage_dmg: Option<f64>,
    /// Population σ of the tracked damage advantages (None if n < 3).
    pub avg_advantage_dmg_sd: Option<f64>,
    /// Mean headcount advantage over fights where it was tracked.
    pub avg_advantage_crew: Option<f64>,
    /// Population σ of the tracked headcount advantages (None if n < 3).
    pub avg_advantage_crew_sd: Option<f64>,
}

/// Win/loss/disengage tally for one enemy category.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CategoryTally {
    pub wins: u32,
    pub losses: u32,
    pub disengages: u32,
}

impl CategoryTally {
    /// Total fights against this category (the sort key).
    fn total(&self) -> u32 {
        self.wins + self.losses + self.disengages
    }
}

/// Five-number summary for a box-and-whiskers plot.
#[derive(Clone, Copy, Debug, PartialEq)]
#[allow(dead_code)] // consumed by the chart rendering (task #11)
pub struct BoxPlot {
    pub min: f64,
    pub q1: f64,
    pub median: f64,
    pub q3: f64,
    pub max: f64,
    /// How many values went into the summary (1 => render a point, not a box).
    pub n: usize,
}

/// Linear-interpolated percentile of an already-sorted slice (numpy "type 7").
fn percentile(sorted: &[f64], p: f64) -> f64 {
    if sorted.len() == 1 {
        return sorted[0];
    }
    let rank = p * (sorted.len() - 1) as f64;
    let lo = rank.floor() as usize;
    let hi = rank.ceil() as usize;
    sorted[lo] + (sorted[hi] - sorted[lo]) * (rank - lo as f64)
}

/// Five-number summary of `values`, or `None` if empty. With one value all five
/// numbers coincide (callers special-case `n == 1` as a point).
#[allow(dead_code)] // consumed by the chart rendering (task #11)
pub fn box_plot(values: &[f64]) -> Option<BoxPlot> {
    if values.is_empty() {
        return None;
    }
    let mut v = values.to_vec();
    v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    Some(BoxPlot {
        min: v[0],
        q1: percentile(&v, 0.25),
        median: percentile(&v, 0.5),
        q3: percentile(&v, 0.75),
        max: v[v.len() - 1],
        n: v.len(),
    })
}

/// Display label for an enemy category.
pub fn category_label(c: &BattleCategory) -> String {
    match c {
        BattleCategory::Brigand => "Brigands and Barbarians".to_string(),
        BattleCategory::BrigandKing(name) => format!("King: {name}"),
        BattleCategory::Vampirate => "Vampirates".to_string(),
        BattleCategory::Skelly => "Skellies".to_string(),
        BattleCategory::Werewolf => "Werewolves".to_string(),
        BattleCategory::Zombie => "Zombies".to_string(),
        BattleCategory::BlackShip => "Black Ship".to_string(),
        BattleCategory::MonkeyBoat => "Monkey Boat".to_string(),
    }
}

/// Aggregate the per-battle records of a voyage. See [`BattleStats`].
#[allow(dead_code)] // consumed by the Voyage Statistics UI (task #7)
pub fn battle_stats(voyage: &Voyage) -> BattleStats {
    let mut s = BattleStats::default();
    let (mut naval, mut boarding, mut total) = (Vec::new(), Vec::new(), Vec::new());
    let (mut adv_dmg, mut adv_crew) = (Vec::new(), Vec::new());
    // Per-fight samples for the means' standard deviations.
    let (mut won_poe, mut net_decisive) = (Vec::new(), Vec::new());
    let (mut won_goods, mut net_goods_decisive) = (Vec::new(), Vec::new());
    for b in &voyage.battles {
        if let Some(a) = b.advantage_dmg {
            adv_dmg.push(a);
        }
        if let Some(a) = b.advantage_crew {
            adv_crew.push(a);
        }
        // Tally enemy categories (insertion-order Vec; few distinct categories),
        // broken down by outcome.
        let label = category_label(&b.category);
        let idx = match s.categories.iter().position(|(l, _)| *l == label) {
            Some(i) => i,
            None => {
                s.categories.push((label, CategoryTally::default()));
                s.categories.len() - 1
            }
        };
        let tally = &mut s.categories[idx].1;
        match b.outcome {
            BattleOutcome::Won => tally.wins += 1,
            BattleOutcome::Lost => tally.losses += 1,
            BattleOutcome::Disengaged => tally.disengages += 1,
            BattleOutcome::Ongoing => {}
        }
        match b.outcome {
            BattleOutcome::Won => {
                s.wins += 1;
                let p = b.poe.unwrap_or(0) as f64;
                let g = b.goods.unwrap_or(0) as f64;
                won_poe.push(p);
                net_decisive.push(p);
                won_goods.push(g);
                net_goods_decisive.push(g);
            }
            BattleOutcome::Lost => {
                s.losses += 1;
                net_decisive.push(b.poe.unwrap_or(0) as f64);
                net_goods_decisive.push(-(b.goods.unwrap_or(0) as f64));
            }
            BattleOutcome::Disengaged => s.disengages += 1,
            BattleOutcome::Ongoing => {}
        }
        if let Some(p) = b.poe {
            s.poe_net_total += p;
            if b.outcome == BattleOutcome::Won && p > 0 {
                s.poe_won_total += p;
            }
        }
        if b.outcome == BattleOutcome::Won {
            if let Some(g) = b.goods {
                s.goods_won_total += g as u64;
            }
        }
        if let Some(t) = b.total_secs() {
            s.time_in_battle_secs += t;
            total.push(t);
        }
        if let Some(t) = b.sea_secs() {
            naval.push(t);
        }
        if let Some(t) = b.boarding_secs() {
            boarding.push(t);
        }
    }
    let mean = |v: &[i64]| (!v.is_empty()).then(|| v.iter().sum::<i64>() as f64 / v.len() as f64);
    s.avg_battle_secs = mean(&total);
    s.avg_battle_sd = stdev_i64(&total);
    s.avg_naval_secs = mean(&naval);
    s.avg_naval_sd = stdev_i64(&naval);
    s.avg_boarding_secs = mean(&boarding);
    s.avg_boarding_sd = stdev_i64(&boarding);
    s.time_at_sea_secs = voyage
        .duration_secs()
        .map(|d| (d - s.time_in_battle_secs).max(0));

    // Means and σ over the per-fight samples (σ only with 3+ data points).
    s.poe_per_fight_won = mean_f64(&won_poe);
    s.poe_per_fight_won_sd = stdev_f64(&won_poe);
    s.poe_per_fight_net = mean_f64(&net_decisive);
    s.poe_per_fight_net_sd = stdev_f64(&net_decisive);
    s.goods_per_fight = mean_f64(&won_goods);
    s.goods_per_fight_sd = stdev_f64(&won_goods);
    s.goods_per_engagement = mean_f64(&net_goods_decisive);
    s.goods_per_engagement_sd = stdev_f64(&net_goods_decisive);

    let decisive = s.wins + s.losses;
    let avg_crew = match (voyage.avg_pirates(), voyage.avg_swabbies()) {
        (Some(p), Some(sw)) => Some(p + sw),
        _ => None,
    }
    .filter(|c| *c > 0.0);
    s.poe_per_crew = avg_crew.map(|c| s.poe_net_total as f64 / c);
    s.poe_per_crew_per_fight_won = avg_crew
        .filter(|_| s.wins > 0)
        .map(|c| s.poe_won_total as f64 / c / s.wins as f64);
    s.poe_per_crew_per_fight_all = avg_crew
        .filter(|_| decisive > 0)
        .map(|c| s.poe_net_total as f64 / c / decisive as f64);

    // Most-fought category first; ties alphabetical for stability.
    s.categories
        .sort_by(|a, b| b.1.total().cmp(&a.1.total()).then_with(|| a.0.cmp(&b.0)));

    s.avg_advantage_dmg = mean_f64(&adv_dmg);
    s.avg_advantage_dmg_sd = stdev_f64(&adv_dmg);
    s.avg_advantage_crew = mean_f64(&adv_crew);
    s.avg_advantage_crew_sd = stdev_f64(&adv_crew);
    s
}

/// Mean of an `f64` sample, or `None` when empty.
fn mean_f64(v: &[f64]) -> Option<f64> {
    (!v.is_empty()).then(|| v.iter().sum::<f64>() / v.len() as f64)
}

/// Population standard deviation of an `f64` sample, or `None` with fewer than
/// three data points (per the Voyage Statistics spec — too few to be meaningful).
fn stdev_f64(v: &[f64]) -> Option<f64> {
    if v.len() < 3 {
        return None;
    }
    let m = v.iter().sum::<f64>() / v.len() as f64;
    let var = v.iter().map(|x| (x - m).powi(2)).sum::<f64>() / v.len() as f64;
    Some(var.sqrt())
}

/// Population standard deviation of an `i64` sample (e.g. durations in seconds).
fn stdev_i64(v: &[i64]) -> Option<f64> {
    let f: Vec<f64> = v.iter().map(|&x| x as f64).collect();
    stdev_f64(&f)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::voyage::{Battle, BattleCategory, BattleOutcome, CrewSample, Voyage};
    use chrono::{NaiveDate, NaiveDateTime};

    fn dt(h: u32, m: u32, s: u32) -> NaiveDateTime {
        NaiveDate::from_ymd_opt(2026, 5, 14)
            .unwrap()
            .and_hms_opt(h, m, s)
            .unwrap()
    }

    fn commodity(id: u64, name: &str) -> Commodity {
        Commodity {
            id,
            name: name.to_string(),
        }
    }

    fn row(id: u64, restock: &str, stock: &str) -> InventoryRow {
        let mut r = InventoryRow::new(id);
        r.restock = restock.to_string();
        r.stock = stock.to_string();
        r
    }

    fn approx(a: f64, b: f64) {
        assert!((a - b).abs() < 1e-9, "expected {b}, got {a}");
    }

    #[test]
    fn consumption_from_stock_delta() {
        let commodities = vec![
            commodity(1, "Small cannon balls"),
            commodity(2, "Grog"),
            commodity(3, "Fine rum"),
            commodity(4, "Rum spice"),
        ];
        let rows = vec![
            row(1, "200", "50"), // 150 balls used
            row(2, "100", "40"), // 60 grog -> x3 = 180 alcohol
            row(3, "20", "5"),   // 15 fine rum -> x6 = 90 alcohol
            row(4, "30", "12"),  // 18 rum spice
        ];
        // One-hour run, 4 battles, constant crew of 5 pirates + 3 swabbies.
        let voy = Voyage {
            sailed_at: Some(dt(12, 0, 0)),
            ported_at: Some(dt(13, 0, 0)),
            battles: vec![Battle::default(); 4],
            crew_samples: vec![CrewSample {
                at: dt(12, 0, 0),
                pirates: 5,
                swabbies: 3,
            }],
            ..Voyage::default()
        };

        let stats = consumption_stats(&voy, &rows, &commodities, Some(CannonSize::Small));
        assert_eq!(stats.balls, Some(150));
        approx(stats.balls_per_battle.unwrap(), 37.5);
        assert_eq!(stats.alcohol, 270); // 180 + 90
        assert_eq!(stats.rum_spice, 18);
        approx(stats.alcohol_per_crew.unwrap(), 270.0 / 8.0); // crew = 5 + 3
        approx(stats.alcohol_per_crew_per_min.unwrap(), 270.0 / 8.0 / 60.0);
        approx(stats.rum_spice_per_swabbie.unwrap(), 6.0); // 18 / 3
        approx(stats.rum_spice_per_swabbie_per_min.unwrap(), 6.0 / 60.0);
    }

    #[test]
    fn no_ship_means_no_ball_count() {
        let commodities = vec![commodity(1, "Small cannon balls")];
        let rows = vec![row(1, "200", "50")];
        let stats = consumption_stats(&Voyage::default(), &rows, &commodities, None);
        assert_eq!(stats.balls, None);
        assert_eq!(stats.balls_per_battle, None);
    }

    #[test]
    fn battle_stats_aggregates_wins_losses_and_loot() {
        let won = Battle {
            outcome: BattleOutcome::Won,
            started_at: Some(dt(12, 0, 0)),
            grappled_at: Some(dt(12, 2, 0)),
            ended_at: Some(dt(12, 5, 0)),
            poe: Some(8000),
            goods: Some(10),
            category: BattleCategory::BrigandKing("Vargas the Mad".to_string()),
            ..Battle::default()
        };
        let lost = Battle {
            outcome: BattleOutcome::Lost,
            started_at: Some(dt(12, 10, 0)),
            ended_at: Some(dt(12, 14, 0)),
            poe: Some(-2000),
            goods: Some(50), // taken from us — not counted as "won"
            ..Battle::default()
        };
        let disengaged = Battle {
            outcome: BattleOutcome::Disengaged,
            started_at: Some(dt(12, 20, 0)),
            ended_at: Some(dt(12, 21, 0)),
            ..Battle::default()
        };
        let voy = Voyage {
            sailed_at: Some(dt(12, 0, 0)),
            ported_at: Some(dt(13, 0, 0)),
            battles: vec![won, lost, disengaged],
            crew_samples: vec![CrewSample {
                at: dt(12, 0, 0),
                pirates: 6,
                swabbies: 4,
            }],
            ..Voyage::default()
        };
        let s = battle_stats(&voy);
        assert_eq!((s.wins, s.losses, s.disengages), (1, 1, 1));
        assert_eq!(s.poe_won_total, 8000);
        assert_eq!(s.poe_net_total, 6000); // 8000 − 2000
        assert_eq!(s.goods_won_total, 10); // wins only
        approx(s.poe_per_fight_won.unwrap(), 8000.0);
        approx(s.poe_per_fight_net.unwrap(), 3000.0); // 6000 / 2 decisive
        approx(s.goods_per_fight.unwrap(), 10.0);
        approx(s.goods_per_engagement.unwrap(), -20.0); // (+10 won, -50 lost) / 2
        approx(s.poe_per_crew.unwrap(), 600.0); // 6000 / 10 crew
        approx(s.poe_per_crew_per_fight_won.unwrap(), 800.0); // 8000 / 10 / 1
        approx(s.poe_per_crew_per_fight_all.unwrap(), 300.0); // 6000 / 10 / 2
        assert_eq!(s.time_in_battle_secs, 600); // 300 + 240 + 60
        assert_eq!(s.time_at_sea_secs, Some(3000)); // 3600 − 600
        approx(s.avg_battle_secs.unwrap(), 200.0); // 600 / 3
        approx(s.avg_naval_secs.unwrap(), 120.0); // only the won fight grappled
        approx(s.avg_boarding_secs.unwrap(), 180.0);
        // Categories: 2 generic Brigand fights (lost + disengaged), 1 king won.
        assert_eq!(
            s.categories,
            vec![
                (
                    "Brigands and Barbarians".to_string(),
                    CategoryTally { wins: 0, losses: 1, disengages: 1 }
                ),
                (
                    "King: Vargas the Mad".to_string(),
                    CategoryTally { wins: 1, losses: 0, disengages: 0 }
                ),
            ]
        );
    }

    #[test]
    fn stdev_needs_three_points() {
        assert_eq!(stdev_f64(&[]), None);
        assert_eq!(stdev_f64(&[1.0, 2.0]), None);
        // Population σ of {2,4,6} is sqrt(8/3) ≈ 1.632993...
        approx(stdev_f64(&[2.0, 4.0, 6.0]).unwrap(), (8.0_f64 / 3.0).sqrt());
    }

    #[test]
    fn battle_stats_attaches_sd_with_three_wins() {
        let win = |poe: i64, secs: u32| Battle {
            outcome: BattleOutcome::Won,
            started_at: Some(dt(12, 0, 0)),
            ended_at: Some(dt(12, 0, secs)),
            poe: Some(poe),
            ..Battle::default()
        };
        let voy = Voyage {
            sailed_at: Some(dt(12, 0, 0)),
            ported_at: Some(dt(13, 0, 0)),
            battles: vec![win(1000, 20), win(2000, 40), win(3000, 50)],
            ..Voyage::default()
        };
        let s = battle_stats(&voy);
        approx(s.poe_per_fight_won.unwrap(), 2000.0);
        // σ of {1000,2000,3000} = sqrt(2_000_000/3) ≈ 816.5
        approx(s.poe_per_fight_won_sd.unwrap(), (2_000_000.0_f64 / 3.0).sqrt());
        assert!(s.avg_battle_sd.is_some()); // three durations
    }

    #[test]
    fn box_plot_five_number_summary() {
        let b = box_plot(&[5.0, 1.0, 3.0, 2.0, 4.0]).unwrap();
        assert_eq!(b.min, 1.0);
        assert_eq!(b.q1, 2.0);
        assert_eq!(b.median, 3.0);
        assert_eq!(b.q3, 4.0);
        assert_eq!(b.max, 5.0);
        assert_eq!(b.n, 5);

        assert!(box_plot(&[]).is_none());

        let one = box_plot(&[7.0]).unwrap();
        assert_eq!((one.min, one.median, one.max, one.n), (7.0, 7.0, 7.0, 1));
    }

    #[test]
    fn time_weighted_crew_average() {
        // First half-hour with 4 swabbies, second half-hour with 8 -> avg 6.
        let voy = Voyage {
            sailed_at: Some(dt(12, 0, 0)),
            ported_at: Some(dt(13, 0, 0)),
            crew_samples: vec![
                CrewSample {
                    at: dt(12, 0, 0),
                    pirates: 1,
                    swabbies: 4,
                },
                CrewSample {
                    at: dt(12, 30, 0),
                    pirates: 1,
                    swabbies: 8,
                },
            ],
            ..Voyage::default()
        };
        approx(voy.avg_swabbies().unwrap(), 6.0);
        approx(voy.avg_pirates().unwrap(), 1.0);
    }
}
