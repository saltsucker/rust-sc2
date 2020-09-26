#[macro_use]
extern crate clap;

use rand::prelude::*;
use rust_sc2::prelude::*;
use std::{cmp::Ordering, collections::HashSet};

#[bot]
#[derive(Default)]
struct ReaperRushAI {
	reapers_retreat: HashSet<u64>,
	last_loop_distributed: u32,
}

impl Player for ReaperRushAI {
	fn on_start(&mut self) -> SC2Result<()> {
		if let Some(townhall) = self.units.my.townhalls.first() {
			// Setting rallypoint for command center
			townhall.smart(Target::Pos(self.start_center), false);

			// Ordering scv on initial 50 minerals
			townhall.train(UnitTypeId::SCV, false);
			self.subtract_resources(UnitTypeId::SCV, true);
		}

		// Splitting workers to closest mineral crystals
		self.units.my.workers.iter().for_each(|u| {
			if let Some(mineral) = self.units.mineral_fields.closest(u) {
				u.gather(mineral.tag, false);
			}
		});

		Ok(())
	}

	fn on_step(&mut self, _iteration: usize) -> SC2Result<()> {
		self.distribute_workers();
		self.build();
		self.train();
		self.execute_micro();
		Ok(())
	}

	fn get_player_settings(&self) -> PlayerSettings {
		PlayerSettings::new(Race::Terran, Some("RustyReapers"))
	}
}

impl ReaperRushAI {
	const DISTRIBUTION_DELAY: u32 = 8;

	fn distribute_workers(&mut self) {
		if self.units.my.workers.is_empty() {
			return;
		}
		let mut idle_workers = self.units.my.workers.idle();

		// Check distribution delay if there aren't any idle workers
		let game_loop = self.state.observation.game_loop;
		let last_loop = &mut self.last_loop_distributed;
		if idle_workers.is_empty() && *last_loop + Self::DISTRIBUTION_DELAY > game_loop {
			return;
		}
		*last_loop = game_loop;

		// Distribute
		let mineral_fields = &self.units.mineral_fields;
		if mineral_fields.is_empty() {
			return;
		}
		let bases = self.units.my.townhalls.ready();
		if bases.is_empty() {
			return;
		}

		let mut deficit_minings = Units::new();
		let mut deficit_geysers = Units::new();

		// Distributing mineral workers
		bases.iter().for_each(
			|base| match base.assigned_harvesters.cmp(&base.ideal_harvesters) {
				Ordering::Less => (0..(base.ideal_harvesters.unwrap() - base.assigned_harvesters.unwrap()))
					.for_each(|_| {
						deficit_minings.push(base.clone());
					}),
				Ordering::Greater => {
					let local_minerals = mineral_fields
						.iter()
						.closer(11.0, base)
						.map(|m| m.tag)
						.collect::<Vec<u64>>();

					idle_workers.extend(
						self.units
							.my
							.workers
							.filter(|u| {
								u.target_tag().map_or(false, |target_tag| {
									local_minerals.contains(&target_tag)
										|| (u.is_carrying_minerals() && target_tag == base.tag)
								})
							})
							.iter()
							.take(
								(base.assigned_harvesters.unwrap() - base.ideal_harvesters.unwrap()) as usize,
							)
							.cloned(),
					);
				}
				_ => {}
			},
		);

		// Distributing gas workers
		self.units
			.my
			.gas_buildings
			.iter()
			.ready()
			.filter(|g| g.vespene_contents.map_or(false, |vespene| vespene > 0))
			.for_each(|gas| match gas.assigned_harvesters.cmp(&gas.ideal_harvesters) {
				Ordering::Less => (0..(gas.ideal_harvesters.unwrap() - gas.assigned_harvesters.unwrap()))
					.for_each(|_| {
						deficit_geysers.push(gas.clone());
					}),
				Ordering::Greater => {
					idle_workers.extend(
						self.units
							.my
							.workers
							.filter(|u| {
								u.target_tag().map_or(false, |target_tag| {
									target_tag == gas.tag
										|| (u.is_carrying_vespene()
											&& target_tag == bases.closest(gas).unwrap().tag)
								})
							})
							.iter()
							.take((gas.assigned_harvesters.unwrap() - gas.ideal_harvesters.unwrap()) as usize)
							.cloned(),
					);
				}
				_ => {}
			});

		// Distributing idle workers
		let minerals_near_base = if idle_workers.len() > deficit_minings.len() + deficit_geysers.len() {
			let minerals = mineral_fields.filter(|m| bases.iter().any(|base| base.is_closer(11.0, *m)));
			if minerals.is_empty() {
				None
			} else {
				Some(minerals)
			}
		} else {
			None
		};

		idle_workers.iter().for_each(|u| {
			if let Some(closest) = deficit_geysers.closest(u) {
				let tag = closest.tag;
				deficit_geysers.remove(tag);
				u.gather(tag, false);
			} else if let Some(closest) = deficit_minings.closest(u) {
				u.gather(
					mineral_fields
						.closer(11.0, closest)
						.max(|m| m.mineral_contents.unwrap_or(0))
						.unwrap()
						.tag,
					false,
				);
				let tag = closest.tag;
				deficit_minings.remove(tag);
			} else if u.is_idle() {
				if let Some(mineral) = minerals_near_base.as_ref().and_then(|ms| ms.closest(u)) {
					u.gather(mineral.tag, false);
				}
			}
		});
	}

	fn get_builder(&self, pos: Point2, mineral_tags: &[u64]) -> Option<&Unit> {
		self.units
			.my
			.workers
			.iter()
			.filter(|u| {
				!(u.is_constructing()
					|| u.is_returning() || u.is_carrying_resource()
					|| (u.is_gathering() && u.target_tag().map_or(true, |tag| !mineral_tags.contains(&tag))))
			})
			.closest(pos)
	}
	fn build(&mut self) {
		if self.minerals < 75 {
			return;
		}

		let mineral_tags = self
			.units
			.mineral_fields
			.iter()
			.map(|u| u.tag)
			.collect::<Vec<u64>>();
		let main_base = self.start_location.towards(self.game_info.map_center, 8.0);

		if self.counter().count(UnitTypeId::Refinery) < 2
			&& self.counter().ordered().count(UnitTypeId::Refinery) == 0
			&& self.can_afford(UnitTypeId::Refinery, false)
		{
			let start_location = self.start_location;
			if let Some(geyser) = self.find_gas_placement(start_location) {
				if let Some(builder) = self.get_builder(geyser.position, &mineral_tags) {
					builder.build_gas(geyser.tag, false);
					self.subtract_resources(UnitTypeId::Refinery, false);
				}
			}
		}

		if self.supply_left < 3
			&& self.supply_cap < 200
			&& self.counter().ordered().count(UnitTypeId::SupplyDepot) == 0
			&& self.can_afford(UnitTypeId::SupplyDepot, false)
		{
			if let Some(location) =
				self.find_placement(UnitTypeId::SupplyDepot, main_base, Default::default())
			{
				if let Some(builder) = self.get_builder(location, &mineral_tags) {
					builder.build(UnitTypeId::SupplyDepot, location, false);
					self.subtract_resources(UnitTypeId::SupplyDepot, false);
					return;
				}
			}
		}

		if self.counter().all().count(UnitTypeId::Barracks) < 4
			&& self.can_afford(UnitTypeId::Barracks, false)
		{
			if let Some(location) = self.find_placement(
				UnitTypeId::Barracks,
				main_base,
				PlacementOptions {
					step: 4,
					..Default::default()
				},
			) {
				if let Some(builder) = self.get_builder(location, &mineral_tags) {
					builder.build(UnitTypeId::Barracks, location, false);
					self.subtract_resources(UnitTypeId::Barracks, false);
				}
			}
		}
	}

	fn train(&mut self) {
		if self.minerals < 50 || self.supply_left == 0 {
			return;
		}

		if self.supply_workers < 22 && self.can_afford(UnitTypeId::SCV, true) {
			if let Some(cc) = self
				.units
				.my
				.townhalls
				.iter()
				.find(|u| u.is_ready() && u.is_almost_idle())
			{
				cc.train(UnitTypeId::SCV, false);
				self.subtract_resources(UnitTypeId::SCV, true);
			}
		}

		if self.can_afford(UnitTypeId::Reaper, true) {
			if let Some(barracks) = self
				.units
				.my
				.structures
				.iter()
				.find(|u| u.type_id == UnitTypeId::Barracks && u.is_ready() && u.is_almost_idle())
			{
				barracks.train(UnitTypeId::Reaper, false);
				self.subtract_resources(UnitTypeId::Reaper, true);
			}
		}
	}

	fn throw_mine(&self, reaper: &Unit, target: &Unit) -> bool {
		if reaper.has_ability(AbilityId::KD8ChargeKD8Charge)
			&& reaper.in_ability_cast_range(AbilityId::KD8ChargeKD8Charge, target, 0.0)
		{
			reaper.command(AbilityId::KD8ChargeKD8Charge, Target::Pos(target.position), false);
			true
		} else {
			false
		}
	}
	fn execute_micro(&mut self) {
		// Lower ready depots
		self.units
			.my
			.structures
			.iter()
			.of_type(UnitTypeId::SupplyDepot)
			.ready()
			.for_each(|s| s.use_ability(AbilityId::MorphSupplyDepotLower, false));

		// Reapers micro
		let reapers = self.units.my.units.of_type(UnitTypeId::Reaper);
		if reapers.is_empty() {
			return;
		}

		let targets = {
			let ground_targets = self.units.enemy.all.ground();
			let ground_attackers = ground_targets.filter(|e| e.can_attack_ground());
			if ground_attackers.is_empty() {
				ground_targets
			} else {
				ground_attackers
			}
		};

		reapers.iter().for_each(|u| {
			let is_retreating = self.reapers_retreat.contains(&u.tag);
			if is_retreating {
				if u.health_percentage().unwrap() > 0.75 {
					self.reapers_retreat.remove(&u.tag);
				}
			} else if u.health_percentage().unwrap() < 0.5 {
				self.reapers_retreat.insert(u.tag);
			}

			match targets.closest(u) {
				Some(closest) => {
					if self.throw_mine(u, closest) {
						return;
					}
					if is_retreating || u.on_cooldown() {
						match targets
							.iter()
							.filter(|t| t.in_range(u, t.speed() + if is_retreating { 2.0 } else { 0.5 }))
							.closest(u)
						{
							Some(closest_attacker) => {
								let flee_position = {
									let pos = u.position.towards(closest_attacker.position, -u.speed());
									if self.is_pathable(pos) {
										pos
									} else {
										*u.position
											.neighbors8()
											.iter()
											.filter(|p| self.is_pathable(**p))
											.furthest(closest_attacker)
											.unwrap_or(&self.start_location)
									}
								};
								u.move_to(Target::Pos(flee_position), false);
							}
							None => {
								if !(is_retreating || u.in_range(&closest, 0.0)) {
									u.move_to(Target::Pos(closest.position), false);
								}
							}
						}
					} else {
						match targets.iter().in_range_of(u, 0.0).min_by_key(|t| t.hits()) {
							Some(target) => u.attack(Target::Tag(target.tag), false),
							None => u.move_to(Target::Pos(closest.position), false),
						}
					}
				}
				None => {
					let pos = if is_retreating {
						u.position
					} else {
						self.enemy_start
					};
					u.move_to(Target::Pos(pos), false);
				}
			}
		});
	}
}

fn main() -> SC2Result<()> {
	let app = clap_app!(RustyReapers =>
		(version: crate_version!())
		(author: crate_authors!())
		(@arg ladder_server: --LadderServer +takes_value)
		(@arg opponent_id: --OpponentId +takes_value)
		(@arg host_port: --GamePort +takes_value)
		(@arg player_port: --StartPort +takes_value)
		(@arg game_step: -s --step
			+takes_value
			default_value("2")
			"Sets game step for bot"
		)
		(@subcommand local =>
			(about: "Runs local game vs Computer")
			(@arg map: -m --map
				+takes_value
			)
			(@arg race: -r --race
				+takes_value
				"Sets opponent race"
			)
			(@arg difficulty: -d --difficulty
				+takes_value
				"Sets opponent diffuculty"
			)
			(@arg ai_build: --("ai-build")
				+takes_value
				"Sets opponent build"
			)
			(@arg sc2_version: --("sc2-version")
				+takes_value
				"Sets sc2 version"
			)
			(@arg save_replay: --("save-replay")
				+takes_value
				"Sets path to save replay"
			)
			(@arg realtime: --realtime "Enables realtime mode")
		)
		(@subcommand human =>
			(about: "Runs game Human vs Bot")
			(@arg map: -m --map
				+takes_value
			)
			(@arg race: -r --race *
				+takes_value
				"Sets human race"
			)
			(@arg name: --name
				+takes_value
				"Sets human name"
			)
			(@arg sc2_version: --("sc2-version")
				+takes_value
				"Sets sc2 version"
			)
			(@arg save_replay: --("save-replay")
				+takes_value
				"Sets path to save replay"
			)
		)
	)
	.get_matches();

	let game_step = match app.value_of("game_step") {
		Some("0") => panic!("game_step must be X >= 1"),
		Some(step) => step.parse::<u32>().expect("Can't parse game_step"),
		None => unreachable!(),
	};

	let mut bot = ReaperRushAI::default();
	bot.set_game_step(game_step);

	if app.is_present("ladder_server") {
		run_ladder_game(
			&mut bot,
			app.value_of("ladder_server").unwrap_or("127.0.0.1"),
			app.value_of("host_port").expect("GamePort must be specified"),
			app.value_of("player_port")
				.expect("StartPort must be specified")
				.parse()
				.expect("Can't parse StartPort"),
			app.value_of("opponent_id"),
		)
	} else {
		let mut rng = thread_rng();

		match app.subcommand() {
			("local", Some(sub)) => run_vs_computer(
				&mut bot,
				Computer::new(
					sub.value_of("race").map_or(Race::Random, |race| {
						race.parse().expect("Can't parse computer race")
					}),
					sub.value_of("difficulty")
						.map_or(Difficulty::VeryEasy, |difficulty| {
							difficulty.parse().expect("Can't parse computer difficulty")
						}),
					sub.value_of("ai_build")
						.map(|ai_build| ai_build.parse().expect("Can't parse computer build")),
				),
				sub.value_of("map").unwrap_or_else(|| {
					[
						"AcropolisLE",
						"DiscoBloodbathLE",
						"EphemeronLE",
						"ThunderbirdLE",
						"TritonLE",
						"WintersGateLE",
						"WorldofSleepersLE",
					]
					.choose(&mut rng)
					.unwrap()
				}),
				LaunchOptions {
					sc2_version: sub.value_of("sc2_version"),
					realtime: sub.is_present("realtime"),
					save_replay_as: sub.value_of("save_replay"),
				},
			),
			("human", Some(sub)) => run_vs_human(
				&mut bot,
				PlayerSettings::new(
					sub.value_of("race")
						.unwrap()
						.parse()
						.expect("Can't parse human race"),
					sub.value_of("name"),
				),
				sub.value_of("map").unwrap_or_else(|| {
					[
						"AcropolisLE",
						"DiscoBloodbathLE",
						"EphemeronLE",
						"ThunderbirdLE",
						"TritonLE",
						"WintersGateLE",
						"WorldofSleepersLE",
					]
					.choose(&mut rng)
					.unwrap()
				}),
				LaunchOptions {
					sc2_version: sub.value_of("sc2_version"),
					realtime: true,
					save_replay_as: sub.value_of("save_replay"),
				},
			),
			_ => {
				println!("Game mode is not specified! Use -h, --help to print help information.");
				std::process::exit(0);
			}
		}
	}
}
