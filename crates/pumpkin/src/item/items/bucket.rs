use std::sync::Arc;
use std::sync::atomic::Ordering;

use crate::{
    entity::{EntityBase, player::Player, r#type::from_type},
    item::{ItemBehaviour, ItemMetadata},
};
use pumpkin_data::{
    Block, BlockDirection, BlockStateId,
    entity::EntityType,
    fluid::Fluid,
    item::Item,
    item_stack::ItemStack,
    sound::{Sound, SoundCategory},
};
use pumpkin_util::{
    GameMode,
    math::{position::BlockPos, vector3::Vector3},
};
use pumpkin_world::{tick::TickPriority, world::BlockFlags};
use uuid::Uuid;

use crate::world::World;

pub struct EmptyBucketItem;
pub struct FilledBucketItem;
pub struct MilkBucketItem;

impl ItemMetadata for EmptyBucketItem {
    fn ids() -> Box<[u16]> {
        [Item::BUCKET.id].into()
    }
}

impl ItemMetadata for FilledBucketItem {
    fn ids() -> Box<[u16]> {
        [
            Item::WATER_BUCKET.id,
            Item::LAVA_BUCKET.id,
            Item::POWDER_SNOW_BUCKET.id,
            Item::AXOLOTL_BUCKET.id,
            Item::COD_BUCKET.id,
            Item::SALMON_BUCKET.id,
            Item::TROPICAL_FISH_BUCKET.id,
            Item::PUFFERFISH_BUCKET.id,
            Item::TADPOLE_BUCKET.id,
        ]
        .into()
    }
}

impl ItemMetadata for MilkBucketItem {
    fn ids() -> Box<[u16]> {
        [Item::MILK_BUCKET.id].into()
    }
}

fn get_start_and_end_pos(player: &Player) -> (Vector3<f64>, Vector3<f64>) {
    let start_pos = player.eye_position();
    let (yaw, pitch) = player.rotation();
    let (yaw_rad, pitch_rad) = (f64::from(yaw.to_radians()), f64::from(pitch.to_radians()));
    let block_interaction_range = 4.5;
    let direction = Vector3::new(
        -yaw_rad.sin() * pitch_rad.cos() * block_interaction_range,
        -pitch_rad.sin() * block_interaction_range,
        pitch_rad.cos() * yaw_rad.cos() * block_interaction_range,
    );

    let end_pos = start_pos.add(&direction);
    (start_pos, end_pos)
}

const fn get_mob_for_bucket(item: &Item) -> Option<(&'static EntityType, Sound)> {
    if item.id == Item::AXOLOTL_BUCKET.id {
        Some((&EntityType::AXOLOTL, Sound::ItemBucketEmptyAxolotl))
    } else if item.id == Item::COD_BUCKET.id {
        Some((&EntityType::COD, Sound::ItemBucketEmptyFish))
    } else if item.id == Item::SALMON_BUCKET.id {
        Some((&EntityType::SALMON, Sound::ItemBucketEmptyFish))
    } else if item.id == Item::TROPICAL_FISH_BUCKET.id {
        Some((&EntityType::TROPICAL_FISH, Sound::ItemBucketEmptyFish))
    } else if item.id == Item::PUFFERFISH_BUCKET.id {
        Some((&EntityType::PUFFERFISH, Sound::ItemBucketEmptyFish))
    } else if item.id == Item::TADPOLE_BUCKET.id {
        Some((&EntityType::TADPOLE, Sound::ItemBucketEmptyTadpole))
    } else {
        None
    }
}

const fn get_empty_sound(item: &Item) -> Sound {
    if let Some((_, sound)) = get_mob_for_bucket(item) {
        sound
    } else if item.id == Item::LAVA_BUCKET.id {
        Sound::ItemBucketEmptyLava
    } else if item.id == Item::POWDER_SNOW_BUCKET.id {
        Sound::ItemBucketEmptyPowderSnow
    } else {
        Sound::ItemBucketEmpty
    }
}

const fn get_fill_sound(item: &Item) -> Sound {
    if item.id == Item::LAVA_BUCKET.id {
        Sound::ItemBucketFillLava
    } else if item.id == Item::POWDER_SNOW_BUCKET.id {
        Sound::ItemBucketFillPowderSnow
    } else {
        Sound::ItemBucketFill
    }
}

fn give_player_bucket_item(player: &Player, item: &'static Item) {
    if player.gamemode.load() == GameMode::Creative {
        let has_item = {
            let inv = player
                .inventory
                .main_inventory
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            inv.iter().any(|stack| stack.item.id == item.id)
        };
        if has_item {
            return;
        }
        let mut item_stack = ItemStack::new(1, item);
        player.inventory.insert_stack_anywhere(&mut item_stack);
    } else {
        let item_stack = ItemStack::new(1, item);
        let mut held_stack = player.inventory.held_item();

        if held_stack.item_count == 1 {
            player.inventory.set_held_item(item_stack);
        } else {
            held_stack.decrement(1);
            player.inventory.set_held_item(held_stack);
            let mut stack_to_give = item_stack;
            let was_added = player.inventory.insert_stack_anywhere(&mut stack_to_give);
            if !was_added && !stack_to_give.is_empty() {
                player
                    .world()
                    .drop_stack(&player.position().to_block_pos(), stack_to_give);
            }
        }
    }
}

/// What filling a bucket is about to do to the world.
///
/// Planning is kept apart from doing for one reason: `PlayerBucketFillEvent`
/// has to be fired **before** the fluid is gone, or cancelling it takes the
/// bucket away from the player and leaves the drained pond drained. A plugin
/// also needs the position that actually changes — it may be the block the
/// player looked at or the one behind it.
enum BucketFill {
    /// Scoop up a powder snow block.
    PowderSnow(BlockPos),
    /// Drain the water out of a waterlogged block.
    Drain(BlockPos),
    /// Take the fluid source itself.
    Source(BlockPos, &'static Item),
}

impl BucketFill {
    const fn position(&self) -> BlockPos {
        match self {
            Self::PowderSnow(pos) | Self::Drain(pos) | Self::Source(pos, _) => *pos,
        }
    }

    const fn item(&self) -> &'static Item {
        match self {
            Self::PowderSnow(_) => &Item::POWDER_SNOW_BUCKET,
            Self::Drain(_) => &Item::WATER_BUCKET,
            Self::Source(_, item) => item,
        }
    }
}

/// What a bucket would pick up at `block_pos` itself, ignoring the clicked face.
fn plan_bucket_fill_at(world: &Arc<World>, block_pos: BlockPos) -> Option<BucketFill> {
    let (block, state) = world.get_block_and_state_id(&block_pos);

    if block == &Block::POWDER_SNOW {
        return Some(BucketFill::PowderSnow(block_pos));
    }

    if block.is_waterlogged(state) {
        return Some(BucketFill::Drain(block_pos));
    }

    if state == Block::LAVA.default_state.id || state == Block::WATER.default_state.id {
        return Some(BucketFill::Source(
            block_pos,
            if state == Block::LAVA.default_state.id {
                &Item::LAVA_BUCKET
            } else {
                &Item::WATER_BUCKET
            },
        ));
    }

    None
}

/// The same, plus the block behind the clicked face: a waterlogged block is
/// drained through the side the player pointed at.
fn plan_bucket_fill(
    world: &Arc<World>,
    block_pos: BlockPos,
    direction: BlockDirection,
) -> Option<BucketFill> {
    if let Some(fill) = plan_bucket_fill_at(world, block_pos) {
        return Some(fill);
    }

    let target_pos = block_pos.offset(direction.to_offset());
    let (block, state) = world.get_block_and_state_id(&target_pos);
    block
        .set_waterlogged(state, false)
        .map(|_| BucketFill::Drain(target_pos))
}

fn apply_bucket_fill(world: &Arc<World>, fill: &BucketFill) {
    match fill {
        BucketFill::PowderSnow(pos) => {
            world.break_block(pos, None, BlockFlags::NOTIFY_ALL | BlockFlags::SKIP_DROPS);
        }
        BucketFill::Drain(pos) => {
            let (block, state) = world.get_block_and_state_id(pos);
            let state_id = block.set_waterlogged(state, false).unwrap_or(state);
            world.set_block_state(pos, state_id, BlockFlags::NOTIFY_ALL);
            world.schedule_fluid_tick(&Fluid::WATER, *pos, 5, TickPriority::Normal);
        }
        BucketFill::Source(pos, _) => {
            world.break_block(pos, None, BlockFlags::NOTIFY_ALL);
            world.set_block_state(pos, Block::AIR.default_state.id, BlockFlags::NOTIFY_ALL);
        }
    }
}

/// Tries to pick up powder snow, a waterlogged block, or a fluid source block at `block_pos`,
/// returning the matching filled bucket item on success.
///
/// Plans and applies in one go: a dispenser fires no player event, so there is
/// nothing to ask in between.
pub(crate) fn try_pickup_fluid_at(
    world: &Arc<World>,
    block_pos: BlockPos,
) -> Option<&'static Item> {
    let fill = plan_bucket_fill_at(world, block_pos)?;
    apply_bucket_fill(world, &fill);
    Some(fill.item())
}

pub(crate) const fn should_evaporate_in_nether(item: &Item, world: &World) -> bool {
    item.id != Item::LAVA_BUCKET.id
        && item.id != Item::POWDER_SNOW_BUCKET.id
        && world.dimension.water_evaporates
}

pub(crate) fn play_bucket_evaporation(world: &Arc<World>, position: &Vector3<f64>) {
    world.play_sound_raw(
        Sound::BlockFireExtinguish as u16,
        SoundCategory::Blocks,
        position,
        0.5,
        (rand::random::<f32>() - rand::random::<f32>()).mul_add(0.8, 2.6),
    );
}

/// What emptying a bucket is about to do to the world.
///
/// Split from doing it for the same reason as [`BucketFill`]:
/// `PlayerBucketEmptyEvent` used to be fired *after* the fluid had already been
/// poured and its `cancelled` flag was never read, so a plugin could not stop
/// anyone from flooding anything. The position matters just as much — the fluid
/// rarely lands in the block the player clicked.
enum BucketEmpty {
    /// Place a powder snow block.
    PowderSnow(BlockPos),
    /// Fill a waterloggable block with water.
    Waterlog(BlockPos, BlockStateId),
    /// Place a fluid source.
    Source(BlockPos, BlockStateId),
}

impl BucketEmpty {
    const fn position(&self) -> BlockPos {
        match self {
            Self::PowderSnow(pos) | Self::Waterlog(pos, _) | Self::Source(pos, _) => *pos,
        }
    }
}

fn plan_bucket_empty(
    world: &Arc<World>,
    item: &Item,
    pos: BlockPos,
    direction: BlockDirection,
) -> Option<BucketEmpty> {
    if item.id == Item::POWDER_SNOW_BUCKET.id {
        let state = world.get_block_state(&pos);
        let target_pos = if state.replaceable() {
            pos
        } else {
            pos.offset(direction.to_offset())
        };
        let target_state = world.get_block_state(&target_pos);
        if !target_state.is_air() && !target_state.is_liquid() && !target_state.replaceable() {
            return None;
        }
        return Some(BucketEmpty::PowderSnow(target_pos));
    }

    let (block, state) = world.get_block_and_state(&pos);
    if item.id == Item::WATER_BUCKET.id && block.is_waterlogged(state.id) {
        return Some(BucketEmpty::Waterlog(
            pos,
            block.set_waterlogged(state.id, true).unwrap_or(state.id),
        ));
    }

    let target_pos = pos.offset(direction.to_offset());
    let (block, state) = world.get_block_and_state(&target_pos);

    if block.is_waterloggable() {
        if item.id == Item::LAVA_BUCKET.id {
            return None;
        }
        return Some(BucketEmpty::Waterlog(
            target_pos,
            block.set_waterlogged(state.id, true).unwrap_or(state.id),
        ));
    }

    if state.id == Block::AIR.default_state.id || state.is_liquid() {
        return Some(BucketEmpty::Source(
            target_pos,
            if item.id == Item::LAVA_BUCKET.id {
                Block::LAVA.default_state.id
            } else {
                Block::WATER.default_state.id
            },
        ));
    }

    None
}

fn apply_bucket_empty(world: &Arc<World>, empty: &BucketEmpty) {
    match empty {
        BucketEmpty::PowderSnow(pos) => {
            world.set_block_state(
                pos,
                Block::POWDER_SNOW.default_state.id,
                BlockFlags::NOTIFY_NEIGHBORS,
            );
        }
        BucketEmpty::Waterlog(pos, state_id) => {
            world.set_block_state(pos, *state_id, BlockFlags::NOTIFY_ALL);
            world.schedule_fluid_tick(&Fluid::WATER, *pos, 5, TickPriority::Normal);
        }
        BucketEmpty::Source(pos, state_id) => {
            world.set_block_state(pos, *state_id, BlockFlags::NOTIFY_ALL);
        }
    }
}

/// Plans and applies in one go, for callers with no player event to fire.
pub(crate) fn try_place_filled_bucket(
    world: &Arc<World>,
    item: &Item,
    pos: BlockPos,
    direction: BlockDirection,
) -> bool {
    let Some(empty) = plan_bucket_empty(world, item, pos, direction) else {
        return false;
    };
    apply_bucket_empty(world, &empty);
    true
}

impl ItemBehaviour for EmptyBucketItem {
    fn normal_use(&self, _block: &Item, player: &Player) {
        let world = player.world();
        let (start_pos, end_pos) = get_start_and_end_pos(player);

        let checker = |pos: &BlockPos, world_inner: &Arc<World>| {
            let state_id = world_inner.get_block_state_id(pos);

            let block = Block::from_state_id(state_id);

            if state_id == Block::AIR.default_state.id {
                return false;
            }

            (block.id != Block::WATER.id && block.id != Block::LAVA.id)
                || ((block.id == Block::WATER.id && state_id == Block::WATER.default_state.id)
                    || (block.id == Block::LAVA.id && state_id == Block::LAVA.default_state.id))
        };

        let Some((block_pos, direction)) = world.raycast(start_pos, end_pos, checker) else {
            return;
        };

        let Some(fill) = plan_bucket_fill(&world, block_pos, direction) else {
            return;
        };

        // Ask BEFORE draining. Fired after the fact, cancelling only took the
        // bucket away from the player: the pond stayed drained either way.
        if let Some(server) = world.server.upgrade()
            && let Some(player_arc) = world.get_player_by_uuid(player.gameprofile.id)
        {
            let mut event =
                crate::plugin::api::events::player::player_bucket::PlayerBucketFillEvent::new(
                    player_arc,
                    fill.position(),
                    fill.item().registry_key.to_string(),
                );
            server.plugin_manager.fire_blocking(&server, &mut event);
            if event.cancelled {
                return;
            }
        }

        apply_bucket_fill(&world, &fill);

        world.play_sound(
            get_fill_sound(fill.item()),
            SoundCategory::Blocks,
            &fill.position().to_f64(),
        );

        give_player_bucket_item(player, fill.item());
    }

    fn use_on_entity(&self, _item: &mut ItemStack, player: &Player, entity: Arc<dyn EntityBase>) {
        let ent = entity.get_entity();
        let entity_type = ent.entity_type;
        if (entity_type == &EntityType::COW
            || entity_type == &EntityType::MOOSHROOM
            || entity_type == &EntityType::GOAT)
            && ent.age.load(Ordering::Relaxed) >= 0
        {
            let world = ent.world.load();
            let sound = if entity_type == &EntityType::GOAT {
                if let Some(goat) = entity
                    .cast_any()
                    .downcast_ref::<crate::entity::passive::goat::GoatEntity>()
                    && goat.is_screaming()
                {
                    Sound::EntityGoatScreamingMilk
                } else {
                    Sound::EntityGoatMilk
                }
            } else {
                Sound::EntityCowMilk
            };
            world.play_sound(sound, SoundCategory::Neutral, &ent.pos.load());
            give_player_bucket_item(player, &Item::MILK_BUCKET);
        }
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

impl ItemBehaviour for FilledBucketItem {
    fn normal_use(&self, item: &Item, player: &Player) {
        let world = player.world();
        let (start_pos, end_pos) = get_start_and_end_pos(player);
        let checker = |pos: &BlockPos, world_inner: &Arc<World>| {
            let state_id = world_inner.get_block_state_id(pos);
            if Fluid::from_state_id(state_id).is_some() {
                return false;
            }
            state_id != Block::AIR.default_state.id
        };

        let Some((pos, direction)) = world.raycast(start_pos, end_pos, checker) else {
            return;
        };

        if should_evaporate_in_nether(item, &world) {
            play_bucket_evaporation(&world, &player.position());
            return;
        }
        let Some(empty) = plan_bucket_empty(&world, item, pos, direction) else {
            return;
        };
        let place_pos = empty.position();

        // Ask BEFORE pouring, and about the block the fluid actually lands in
        // — it is the one behind the clicked face far more often than not.
        // Fired after the fact and with its `cancelled` never read, this event
        // protected nothing at all. Reading the position back off the world
        // after the change cannot work either: by then the block already looks
        // like what the bucket made of it.
        if let Some(server) = world.server.upgrade()
            && let Some(player_arc) = world.get_player_by_uuid(player.gameprofile.id)
        {
            let mut event =
                crate::plugin::api::events::player::player_bucket::PlayerBucketEmptyEvent::new(
                    player_arc,
                    place_pos,
                    item.registry_key.to_string(),
                );
            server.plugin_manager.fire_blocking(&server, &mut event);
            if event.cancelled {
                return;
            }
        }

        apply_bucket_empty(&world, &empty);

        world.play_sound(
            get_empty_sound(item),
            SoundCategory::Blocks,
            &place_pos.to_f64(),
        );

        if let Some((entity_type, _)) = get_mob_for_bucket(item) {
            let spawn_coord = Vector3::new(
                f64::from(place_pos.0.x) + 0.5,
                f64::from(place_pos.0.y),
                f64::from(place_pos.0.z) + 0.5,
            );
            let mob = from_type(entity_type, spawn_coord, &world, Uuid::new_v4());
            world.spawn_entity(mob);
        }

        if player.gamemode.load() != GameMode::Creative {
            let item_stack = ItemStack::new(1, &Item::BUCKET);
            player
                .inventory
                .set_slot(player.inventory.get_selected_slot() as usize, item_stack);
        }
    }

    fn use_on_entity(&self, item: &mut ItemStack, player: &Player, entity: Arc<dyn EntityBase>) {
        if item.item.id == Item::WATER_BUCKET.id {
            let entity_type = entity.get_entity().entity_type;
            let result_item = if entity_type == &EntityType::AXOLOTL {
                Some((&Item::AXOLOTL_BUCKET, Sound::ItemBucketFillAxolotl))
            } else if entity_type == &EntityType::COD {
                Some((&Item::COD_BUCKET, Sound::ItemBucketFillFish))
            } else if entity_type == &EntityType::SALMON {
                Some((&Item::SALMON_BUCKET, Sound::ItemBucketFillFish))
            } else if entity_type == &EntityType::TROPICAL_FISH {
                Some((&Item::TROPICAL_FISH_BUCKET, Sound::ItemBucketFillFish))
            } else if entity_type == &EntityType::PUFFERFISH {
                Some((&Item::PUFFERFISH_BUCKET, Sound::ItemBucketFillFish))
            } else if entity_type == &EntityType::TADPOLE {
                Some((&Item::TADPOLE_BUCKET, Sound::ItemBucketFillTadpole))
            } else {
                None
            };

            if let Some((mob_bucket, sound)) = result_item {
                let ent = entity.get_entity();
                let world = ent.world.load();
                world.play_sound(sound, SoundCategory::Neutral, &ent.pos.load());
                give_player_bucket_item(player, mob_bucket);
                ent.remove();
            }
        }
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

impl ItemBehaviour for MilkBucketItem {
    fn normal_use(&self, _item: &Item, player: &Player) {
        let stack = player.inventory().held_item();
        player
            .living_entity
            .set_active_hand(pumpkin_util::Hand::Right, stack, 32);
    }

    fn on_stopped_using(&self, _stack: &ItemStack, player: &Player) {
        player.living_entity.reset_effects_and_attributes();
        give_player_bucket_item(player, &Item::BUCKET);
    }

    fn get_use_duration(&self) -> i32 {
        32
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}
