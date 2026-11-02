//! Durable placement metadata. Block and item bodies remain owned by one live tree.
use super::{BlockId, ListItemPlacement, PendingListItemMove, Placement, block_id_from_op};
use crate::codec::MovedBlockWire;
use crate::core::{OpId, StateVector};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

/// Identity namespaces for topology nodes; block destination kinds are checked separately.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum LogicalContainer {
    Block(BlockId),
    Item(BlockId),
}

/// A logical identity qualified by its creation operation, not its latest move.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct LogicalTarget {
    pub container: LogicalContainer,
    pub origin: OpId,
}

/// Durable deletion context. The physical target is bound to its real incarnation
/// independently of the author's captured subtree, which can be empty for a future target.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubtreeDeletion {
    pub id: OpId,
    pub target: LogicalTarget,
    pub observed: StateVector,
    pub subtree: Vec<LogicalTarget>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlockPlacement {
    pub parent: Option<LogicalContainer>,
    pub parent_origin: Option<OpId>,
    pub id: OpId,
    pub after: Option<OpId>,
    pub right_origin: Option<OpId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlockMove {
    pub id: OpId,
    pub observed: StateVector,
    pub parent: Option<LogicalContainer>,
    pub parent_origin: Option<OpId>,
    pub blocks: Vec<MovedBlockWire>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ItemPlacement {
    pub list_id: BlockId,
    pub list_origin: OpId,
    pub id: OpId,
    pub after: Option<OpId>,
    pub right_origin: Option<OpId>,
}

pub(crate) struct ItemPlacementState {
    pub(crate) value: ListItemPlacement,
    pub(crate) id: OpId,
    pub(crate) observed: StateVector,
    pub(crate) conflicts: Vec<crate::core::causal::CausalCandidate<ListItemPlacement>>,
    pub(crate) implicit: bool,
}

pub(crate) struct TopologyProjection {
    pub(crate) blocks: BTreeMap<BlockId, BlockPlacement>,
    pub(crate) items: BTreeMap<BlockId, ItemPlacement>,
}

/// Placement choices, aliases and creation incarnations; never another content copy.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlockTopology {
    pub initial: BTreeMap<BlockId, BlockPlacement>,
    pub moves: Vec<BlockMove>,
    pub item_aliases: Vec<(OpId, BlockId)>,
    pub deleted: BTreeSet<BlockId>,
    pub initial_items: BTreeMap<BlockId, ItemPlacement>,
    pub item_moves: Vec<PendingListItemMove>,
    pub deleted_items: BTreeSet<BlockId>,
    pub block_origins: Vec<(OpId, BlockId)>,
    pub item_origins: Vec<(OpId, BlockId)>,
    pub deletions: Vec<SubtreeDeletion>,
    pub direct_deleted: BTreeSet<LogicalTarget>,
    /// Pre-session seed incarnations are known without appearing in the sync frontier.
    pub imported_origins: BTreeSet<LogicalTarget>,
}

impl BlockTopology {
    pub(crate) fn validate(&self, frontier: &StateVector) -> Result<(), &'static str> {
        if self.initial.values().any(|p| p.id.counter == 0)
            || self.initial_items.values().any(|p| p.id.counter == 0)
            || self.item_aliases.windows(2).any(|p| p[0].0 >= p[1].0)
            || self.block_origins.windows(2).any(|p| p[0].0 >= p[1].0)
            || self.item_origins.windows(2).any(|p| p[0].0 >= p[1].0)
            || self.moves.windows(2).any(|p| p[0].id >= p[1].id)
            || self.item_moves.windows(2).any(|p| p[0].id >= p[1].id)
            || self.deletions.windows(2).any(|p| p[0].id >= p[1].id)
        {
            return Err("invalid placement identity order");
        }
        let events: BTreeMap<_, _> = self
            .moves
            .iter()
            .map(|m| (m.id, &m.observed))
            .chain(self.item_moves.iter().map(|m| (m.id, &m.observed)))
            .chain(self.deletions.iter().map(|d| (d.id, &d.observed)))
            .collect();
        if events.len() != self.moves.len() + self.item_moves.len() + self.deletions.len() {
            return Err("duplicate placement transaction identity");
        }
        let mut placements = BTreeMap::new();
        for (element, logical) in &self.block_origins {
            if element.counter == 0
                || placements
                    .insert(*element, LogicalContainer::Block(*logical))
                    .is_some()
            {
                return Err("duplicate or invalid block creation identity");
            }
        }
        for (element, logical) in &self.item_origins {
            if element.counter == 0
                || placements
                    .insert(*element, LogicalContainer::Item(*logical))
                    .is_some()
            {
                return Err("duplicate or invalid item creation identity");
            }
        }
        if self
            .initial
            .iter()
            .any(|(logical, p)| placements.get(&p.id) != Some(&LogicalContainer::Block(*logical)))
            || self.initial_items.iter().any(|(logical, p)| {
                placements.get(&p.id) != Some(&LogicalContainer::Item(*logical))
            })
        {
            return Err("invalid current placement incarnation");
        }
        for movement in &self.moves {
            let Some(first) = movement.blocks.first() else {
                return Err("empty placement transaction");
            };
            if first.id.counter == 0
                || movement.blocks.last().map(|b| b.id) != Some(movement.id)
                || movement.blocks.iter().enumerate().any(|(offset, b)| {
                    b.id.peer != movement.id.peer
                        || first.id.counter.checked_add(offset as u64) != Some(b.id.counter)
                })
                || movement.observed.get(movement.id.peer).unwrap_or(0) >= first.id.counter
                || movement.id.counter > frontier.get(movement.id.peer).unwrap_or(0)
                || movement
                    .observed
                    .iter()
                    .any(|(peer, counter)| counter > frontier.get(peer).unwrap_or(0))
            {
                return Err("invalid placement transaction");
            }
            let unique: BTreeSet<_> = movement.blocks.iter().map(|b| b.block_id).collect();
            if unique.len() != movement.blocks.len() {
                return Err("duplicate logical block in placement transaction");
            }
            for block in &movement.blocks {
                if placements
                    .insert(block.id, LogicalContainer::Block(block.block_id))
                    .is_some()
                {
                    return Err("reused block placement identity");
                }
            }
        }
        for movement in &self.item_moves {
            if movement.id.counter == 0
                || movement.id.counter > frontier.get(movement.id.peer).unwrap_or(0)
                || movement.observed.get(movement.id.peer).unwrap_or(0) >= movement.id.counter
                || movement
                    .observed
                    .iter()
                    .any(|(peer, counter)| counter > frontier.get(peer).unwrap_or(0))
            {
                return Err("invalid item placement transaction");
            }
            if placements
                .insert(movement.id, LogicalContainer::Item(movement.item_id))
                .is_some()
            {
                return Err("reused item placement identity");
            }
        }
        if self.item_aliases.iter().any(|(element, logical)| {
            placements.get(element) != Some(&LogicalContainer::Item(*logical))
        }) {
            return Err("invalid logical item placement alias");
        }
        let parent_valid =
            |parent: Option<LogicalContainer>, origin: Option<OpId>| match (parent, origin) {
                (None, None) => true,
                (Some(container), Some(origin)) => {
                    self.valid_target_reference(LogicalTarget { container, origin })
                }
                _ => false,
            };
        if self
            .initial
            .values()
            .any(|p| !parent_valid(p.parent, p.parent_origin))
            || self
                .moves
                .iter()
                .any(|m| !parent_valid(m.parent, m.parent_origin))
            || self.initial_items.values().any(|p| {
                !self.valid_target_reference(LogicalTarget {
                    container: LogicalContainer::Block(p.list_id),
                    origin: p.list_origin,
                })
            })
        {
            return Err("invalid placement parent incarnation");
        }
        for deletion in &self.deletions {
            if deletion.id.counter == 0
                || deletion.id.counter > frontier.get(deletion.id.peer).unwrap_or(0)
                || deletion.observed.get(deletion.id.peer).unwrap_or(0) >= deletion.id.counter
                || deletion
                    .observed
                    .iter()
                    .any(|(peer, counter)| counter > frontier.get(peer).unwrap_or(0))
                || placements.contains_key(&deletion.id)
                || !self.valid_target_reference(deletion.target)
                || self.resolve_deletion_target(
                    Some(deletion.target.container),
                    deletion.target.origin,
                ) != Some(deletion.target)
                || deletion
                    .subtree
                    .first()
                    .is_some_and(|root| *root != deletion.target)
            {
                return Err("invalid captured subtree deletion");
            }
            let mut unique = BTreeSet::new();
            if deletion.subtree.iter().any(|target| {
                target.origin.counter == 0
                    || !unique.insert(target.container)
                    || !self.valid_target_reference(*target)
                    || (match target.container {
                        LogicalContainer::Block(id) => {
                            !self.block_origins.contains(&(target.origin, id))
                        }
                        LogicalContainer::Item(id) => {
                            !self.item_origins.contains(&(target.origin, id))
                        }
                    } && target.origin.counter > frontier.get(target.origin.peer).unwrap_or(0))
            }) {
                return Err("invalid captured subtree incarnation");
            }
        }
        if self.imported_origins.iter().any(|target| {
            self.resolve_deletion_target(Some(target.container), target.origin) != Some(*target)
        }) {
            return Err("invalid imported placement incarnation");
        }
        if self
            .direct_deleted
            .iter()
            .any(|target| target.origin.counter == 0 || !self.valid_target_reference(*target))
        {
            return Err("invalid direct deletion incarnation");
        }
        for start in self
            .initial
            .keys()
            .copied()
            .map(LogicalContainer::Block)
            .chain(
                self.initial_items
                    .keys()
                    .copied()
                    .map(LogicalContainer::Item),
            )
        {
            let mut path = BTreeSet::new();
            let mut current = Some(start);
            while let Some(node) = current {
                if !path.insert(node) {
                    return Err("cyclic initial placements");
                }
                current = match node {
                    LogicalContainer::Block(id) => self.initial.get(&id).and_then(|p| {
                        let target = LogicalTarget {
                            container: p.parent?,
                            origin: p.parent_origin?,
                        };
                        (self.current_target(target.container) == Some(target))
                            .then_some(target.container)
                    }),
                    LogicalContainer::Item(id) => self.initial_items.get(&id).and_then(|p| {
                        let target = LogicalTarget {
                            container: LogicalContainer::Block(p.list_id),
                            origin: p.list_origin,
                        };
                        (self.current_target(target.container) == Some(target))
                            .then_some(target.container)
                    }),
                };
            }
        }
        let mut pending: BTreeSet<_> = events.keys().copied().collect();
        while !pending.is_empty() {
            let ready: Vec<_> = pending
                .iter()
                .copied()
                .filter(|id| {
                    pending.iter().all(|other| {
                        other == id || events[id].get(other.peer).unwrap_or(0) < other.counter
                    })
                })
                .collect();
            if ready.is_empty() {
                return Err("cyclic placement observations");
            }
            for id in ready {
                pending.remove(&id);
            }
        }
        let mut normalized = self.clone();
        normalized.recompute_deletions();
        if normalized.deleted != self.deleted || normalized.deleted_items != self.deleted_items {
            return Err("inconsistent captured deletion projection");
        }
        Ok(())
    }

    pub(crate) fn block_for_element(&self, element: OpId) -> Option<BlockId> {
        let logical = block_id_from_op(element);
        if self.initial.get(&logical).is_some_and(|p| p.id == element) {
            return Some(logical);
        }
        self.block_origins
            .iter()
            .find_map(|(id, logical)| (*id == element).then_some(*logical))
            .or_else(|| {
                self.moves
                    .iter()
                    .flat_map(|m| &m.blocks)
                    .find_map(|m| (m.id == element).then_some(m.block_id))
            })
    }

    pub(crate) fn record_initial_tree(&mut self, block: &super::Block, placement: BlockPlacement) {
        record_metadata(block, placement, self, false);
    }

    pub(crate) fn resolve_deletion_target(
        &self,
        expected: Option<LogicalContainer>,
        element: OpId,
    ) -> Option<LogicalTarget> {
        let container = match expected {
            Some(container) => container,
            None => LogicalContainer::Block(self.block_for_element(element)?),
        };
        self.reference_target(container, element)
    }

    pub(crate) fn container_for_element(&self, element: OpId) -> Option<LogicalContainer> {
        self.block_for_element(element)
            .map(LogicalContainer::Block)
            .or_else(|| {
                if let Ok(index) = self
                    .item_origins
                    .binary_search_by_key(&element, |(id, _)| *id)
                {
                    Some(LogicalContainer::Item(self.item_origins[index].1))
                } else {
                    self.item_aliases
                        .binary_search_by_key(&element, |(id, _)| *id)
                        .ok()
                        .map(|index| LogicalContainer::Item(self.item_aliases[index].1))
                }
            })
    }

    pub(crate) fn record_creation(
        &mut self,
        logical: BlockId,
        placement: BlockPlacement,
        replace: bool,
    ) {
        if !self.initial.contains_key(&logical) || replace {
            if let Err(index) = self
                .block_origins
                .binary_search_by_key(&placement.id, |(id, _)| *id)
            {
                self.block_origins.insert(index, (placement.id, logical));
            }
            self.initial.insert(logical, placement);
            self.deleted.remove(&logical);
        }
    }

    pub(crate) fn record_item_creation(
        &mut self,
        logical: BlockId,
        placement: ItemPlacement,
        replace: bool,
    ) {
        if !self.initial_items.contains_key(&logical) || replace {
            if let Err(index) = self
                .item_origins
                .binary_search_by_key(&placement.id, |(id, _)| *id)
            {
                self.item_origins.insert(index, (placement.id, logical));
            }
            self.initial_items.insert(logical, placement);
            self.deleted_items.remove(&logical);
        }
    }

    pub(crate) fn current_target(&self, container: LogicalContainer) -> Option<LogicalTarget> {
        let origin = match container {
            LogicalContainer::Block(id) => self.initial.get(&id)?.id,
            LogicalContainer::Item(id) => self.initial_items.get(&id)?.id,
        };
        Some(LogicalTarget { container, origin })
    }

    pub(crate) fn reference_target(
        &self,
        container: LogicalContainer,
        element: OpId,
    ) -> Option<LogicalTarget> {
        let origin = match container {
            LogicalContainer::Block(id) => self.incarnation(id, element)?,
            LogicalContainer::Item(id) => self.item_incarnation(id, element)?,
        };
        Some(LogicalTarget { container, origin })
    }

    pub(crate) fn live_target(&self, target: LogicalTarget) -> bool {
        self.current_target(target.container) == Some(target)
            && match target.container {
                LogicalContainer::Block(id) => !self.deleted.contains(&id),
                LogicalContainer::Item(id) => !self.deleted_items.contains(&id),
            }
    }

    /// Captured incarnations die; only unseen initial births propagate deletion.
    /// Moves of unrelated outside content into a deleted container do not.
    pub(crate) fn recompute_deletions(&mut self) {
        let mut removed = self.direct_deleted.clone();
        for deletion in &self.deletions {
            let mut subtree: BTreeSet<_> = std::iter::once(deletion.target)
                .chain(deletion.subtree.iter().copied())
                .collect();
            loop {
                let before = subtree.len();
                for (logical, p) in &self.initial {
                    if self.imported_origins.contains(&LogicalTarget {
                        container: LogicalContainer::Block(*logical),
                        origin: p.id,
                    }) || deletion.observed.get(p.id.peer).unwrap_or(0) >= p.id.counter
                    {
                        continue;
                    }
                    let parent = p
                        .parent
                        .zip(p.parent_origin)
                        .map(|(container, origin)| LogicalTarget { container, origin });
                    if parent.is_some_and(|parent| subtree.contains(&parent)) {
                        subtree.insert(LogicalTarget {
                            container: LogicalContainer::Block(*logical),
                            origin: p.id,
                        });
                    }
                }
                for (logical, p) in &self.initial_items {
                    if self.imported_origins.contains(&LogicalTarget {
                        container: LogicalContainer::Item(*logical),
                        origin: p.id,
                    }) || deletion.observed.get(p.id.peer).unwrap_or(0) >= p.id.counter
                    {
                        continue;
                    }
                    if subtree.contains(&LogicalTarget {
                        container: LogicalContainer::Block(p.list_id),
                        origin: p.list_origin,
                    }) {
                        subtree.insert(LogicalTarget {
                            container: LogicalContainer::Item(*logical),
                            origin: p.id,
                        });
                    }
                }
                if subtree.len() == before {
                    break;
                }
            }
            removed.extend(subtree);
        }
        self.deleted.clear();
        self.deleted_items.clear();
        for target in removed {
            if self.current_target(target.container) != Some(target) {
                continue;
            }
            match target.container {
                LogicalContainer::Block(id) => {
                    self.deleted.insert(id);
                }
                LogicalContainer::Item(id) => {
                    self.deleted_items.insert(id);
                }
            }
        }
    }

    pub(crate) fn valid_target_reference(&self, target: LogicalTarget) -> bool {
        match target.container {
            LogicalContainer::Block(logical) => {
                if self
                    .block_origins
                    .iter()
                    .any(|(id, block)| *id == target.origin && *block == logical)
                {
                    return true;
                }
                !self
                    .block_origins
                    .iter()
                    .any(|(_, block)| *block == logical)
                    && self.block_for_element(target.origin).is_none()
                    && !self.item_aliases.iter().any(|(id, _)| *id == target.origin)
                    && logical == block_id_from_op(target.origin)
            }
            LogicalContainer::Item(logical) => {
                if self
                    .item_origins
                    .iter()
                    .any(|(id, item)| *id == target.origin && *item == logical)
                {
                    return true;
                }
                !self.item_origins.iter().any(|(_, item)| *item == logical)
                    && !self.item_aliases.iter().any(|(id, _)| *id == target.origin)
                    && self.block_for_element(target.origin).is_none()
                    && logical == block_id_from_op(target.origin)
            }
        }
    }

    fn incarnation(&self, logical: BlockId, mut element: OpId) -> Option<OpId> {
        for _ in 0..=self.moves.len() {
            if self
                .block_origins
                .iter()
                .any(|(id, block)| *id == element && *block == logical)
            {
                return Some(element);
            }
            element = self
                .moves
                .iter()
                .flat_map(|m| &m.blocks)
                .find(|m| m.id == element && m.block_id == logical)?
                .target;
        }
        None
    }

    fn item_incarnation(&self, logical: BlockId, mut element: OpId) -> Option<OpId> {
        for _ in 0..=self.item_moves.len() {
            if self
                .item_origins
                .iter()
                .any(|(id, item)| *id == element && *item == logical)
            {
                return Some(element);
            }
            element = self
                .item_moves
                .iter()
                .find(|m| m.id == element && m.item_id == logical)?
                .target;
        }
        None
    }

    fn item_register(
        &self,
        logical: BlockId,
        rejected: &BTreeSet<OpId>,
    ) -> Option<ItemPlacementState> {
        let initial = self.initial_items.get(&logical)?;
        let mut state = ItemPlacementState {
            value: ListItemPlacement {
                list_id: initial.list_id,
                placement: Placement {
                    after: initial.after,
                    right_origin: initial.right_origin,
                },
            },
            id: initial.id,
            observed: StateVector::new(),
            conflicts: Vec::new(),
            implicit: true,
        };
        for movement in self.item_moves.iter().filter(|m| {
            m.item_id == logical
                && !rejected.contains(&m.id)
                && self.item_incarnation(logical, m.target) == Some(initial.id)
        }) {
            let value = ListItemPlacement {
                list_id: movement.list_id,
                placement: Placement {
                    after: movement.after,
                    right_origin: movement.right_origin,
                },
            };
            if state.implicit {
                state.value = value;
                state.id = movement.id;
                state.observed = movement.observed.clone();
                state.implicit = false;
            } else {
                crate::core::causal::apply_write(
                    &mut state.value,
                    &mut state.id,
                    &mut state.observed,
                    &mut state.conflicts,
                    crate::core::causal::CausalCandidate {
                        value,
                        op_id: movement.id,
                        observed: movement.observed.clone(),
                    },
                );
            }
        }
        Some(state)
    }

    /// Project placement metadata at structural boundaries, without replaying bodies.
    pub(crate) fn project(&self) -> TopologyProjection {
        let events: BTreeMap<_, _> = self
            .moves
            .iter()
            .map(|m| (m.id, &m.observed))
            .chain(self.item_moves.iter().map(|m| (m.id, &m.observed)))
            .collect();
        let mut ranks = BTreeMap::<OpId, usize>::new();
        let mut remaining: BTreeSet<_> = events.keys().copied().collect();
        while !remaining.is_empty() {
            let ready: Vec<_> = remaining
                .iter()
                .copied()
                .filter(|id| {
                    remaining.iter().all(|other| {
                        other == id || events[id].get(other.peer).unwrap_or(0) < other.counter
                    })
                })
                .collect();
            assert!(!ready.is_empty(), "cyclic placement observations");
            for id in ready {
                let rank = ranks
                    .iter()
                    .filter(|(other, _)| events[&id].get(other.peer).unwrap_or(0) >= other.counter)
                    .map(|(_, rank)| rank + 1)
                    .max()
                    .unwrap_or(1);
                ranks.insert(id, rank);
                remaining.remove(&id);
            }
        }
        let mut ordered: Vec<_> = self.moves.iter().collect();
        ordered.sort_by_key(|m| (ranks[&m.id], m.id));
        let unavailable_items: BTreeSet<_> = self
            .item_moves
            .iter()
            .filter(|m| {
                self.reference_target(LogicalContainer::Block(m.list_id), m.to_list_elem)
                    .is_none_or(|target| !self.live_target(target))
            })
            .map(|m| m.id)
            .collect();
        let mut rejected = unavailable_items.clone();
        let mut pinned = BTreeSet::new();
        let mut forced_roots = BTreeSet::new();
        'projection: loop {
            let mut selected = self.initial.clone();
            let mut items = self.initial_items.clone();
            let mut owners = BTreeMap::<LogicalContainer, OpId>::new();
            for movement in ordered.iter().rev() {
                let destination_live = match (movement.parent, movement.parent_origin) {
                    (None, None) => true,
                    (Some(container), Some(origin)) => {
                        self.live_target(LogicalTarget { container, origin })
                    }
                    _ => false,
                };
                if !destination_live
                    || rejected.contains(&movement.id)
                    || movement.blocks.iter().any(|m| {
                        owners.contains_key(&LogicalContainer::Block(m.block_id))
                            || self.deleted.contains(&m.block_id)
                            || self.initial.get(&m.block_id).is_none_or(|p| {
                                self.incarnation(m.block_id, m.target) != Some(p.id)
                            })
                    })
                {
                    continue;
                }
                for block in &movement.blocks {
                    owners.insert(LogicalContainer::Block(block.block_id), movement.id);
                    selected.insert(
                        block.block_id,
                        BlockPlacement {
                            parent: movement.parent,
                            parent_origin: movement.parent_origin,
                            id: block.id,
                            after: block.after,
                            right_origin: block.right_origin,
                        },
                    );
                }
            }
            for logical in self
                .initial_items
                .keys()
                .filter(|id| !self.deleted_items.contains(*id))
            {
                let state = self
                    .item_register(*logical, &rejected)
                    .expect("initial item");
                let initial = self.initial_items[logical];
                if state.implicit
                    && !self.live_target(LogicalTarget {
                        container: LogicalContainer::Block(initial.list_id),
                        origin: initial.list_origin,
                    })
                    && let Some(candidate) = self
                        .item_moves
                        .iter()
                        .filter(|m| {
                            m.item_id == *logical
                                && !unavailable_items.contains(&m.id)
                                && self.item_incarnation(*logical, m.target) == Some(initial.id)
                        })
                        .max_by_key(|m| (ranks[&m.id], m.id))
                {
                    rejected.remove(&candidate.id);
                    pinned.insert(candidate.id);
                    continue 'projection;
                }
                if !state.implicit {
                    let movement = self
                        .item_moves
                        .iter()
                        .find(|m| m.id == state.id)
                        .expect("selected item history");
                    let destination = self
                        .reference_target(
                            LogicalContainer::Block(movement.list_id),
                            movement.to_list_elem,
                        )
                        .expect("viable item destination");
                    owners.insert(LogicalContainer::Item(*logical), state.id);
                    items.insert(
                        *logical,
                        ItemPlacement {
                            list_id: state.value.list_id,
                            list_origin: destination.origin,
                            id: state.id,
                            after: state.value.placement.after,
                            right_origin: state.value.placement.right_origin,
                        },
                    );
                }
            }
            for (logical, placement) in &mut selected {
                if self.deleted.contains(logical) {
                    continue;
                }
                if forced_roots.contains(logical)
                    || placement.parent.is_some_and(|container| {
                        placement.parent_origin.is_none_or(|origin| {
                            !self.live_target(LogicalTarget { container, origin })
                        })
                    })
                {
                    placement.parent = None;
                    placement.parent_origin = None;
                }
            }
            let mut reject = None;
            let mut force_root = None;
            for start in selected
                .keys()
                .copied()
                .map(LogicalContainer::Block)
                .chain(items.keys().copied().map(LogicalContainer::Item))
                .filter(|node| match node {
                    LogicalContainer::Block(id) => !self.deleted.contains(id),
                    LogicalContainer::Item(id) => !self.deleted_items.contains(id),
                })
            {
                let mut path = Vec::new();
                let mut current = start;
                loop {
                    if let Some(index) = path.iter().position(|id| *id == current) {
                        reject = path[index..]
                            .iter()
                            .filter_map(|id| owners.get(id))
                            .filter(|id| !pinned.contains(*id))
                            .min_by_key(|id| (ranks[&**id], **id))
                            .copied();
                        if reject.is_none() {
                            force_root = path[index..]
                                .iter()
                                .filter_map(|node| match node {
                                    LogicalContainer::Block(id) => Some(*id),
                                    _ => None,
                                })
                                .min();
                        }
                        break;
                    }
                    path.push(current);
                    let next = match current {
                        LogicalContainer::Block(id) => selected.get(&id).and_then(|p| {
                            let target = LogicalTarget {
                                container: p.parent?,
                                origin: p.parent_origin?,
                            };
                            self.live_target(target).then_some(target.container)
                        }),
                        LogicalContainer::Item(id) => items.get(&id).and_then(|p| {
                            let target = LogicalTarget {
                                container: LogicalContainer::Block(p.list_id),
                                origin: p.list_origin,
                            };
                            self.live_target(target).then_some(target.container)
                        }),
                    };
                    let Some(next) = next else {
                        break;
                    };
                    current = next;
                }
                if reject.is_some() || force_root.is_some() {
                    break;
                }
            }
            if let Some(id) = reject {
                rejected.insert(id);
            } else if let Some(id) = force_root {
                forced_roots.insert(id);
            } else {
                return TopologyProjection {
                    blocks: selected,
                    items,
                };
            }
        }
    }
}

impl super::Document {
    pub(crate) fn logical_block_for_elem(&self, element: OpId) -> Option<BlockId> {
        self.find_block(element)
            .map(|b| b.id)
            .or_else(|| self.blocks.block_topology.block_for_element(element))
    }

    pub(crate) fn logical_item_for_elem(&self, element: OpId) -> Option<BlockId> {
        self.find_list_item(element)
            .map(|item| item.id)
            .or_else(|| {
                self.blocks
                    .block_topology
                    .item_aliases
                    .iter()
                    .find_map(|(id, logical)| (*id == element).then_some(*logical))
            })
    }

    pub(crate) fn logical_container(&self, element: OpId) -> Option<LogicalContainer> {
        self.logical_block_for_elem(element)
            .map(LogicalContainer::Block)
            .or_else(|| {
                self.logical_item_for_elem(element)
                    .map(LogicalContainer::Item)
            })
    }

    pub(crate) fn resolve_container_elem(&self, element: OpId) -> Option<OpId> {
        match self.logical_container(element)? {
            LogicalContainer::Block(id) => {
                let current = self.block_elem_id(id)?;
                if element == current {
                    return Some(current);
                }
                let initial = self.blocks.block_topology.initial.get(&id)?;
                (self.blocks.block_topology.incarnation(id, element) == Some(initial.id))
                    .then_some(current)
            }
            LogicalContainer::Item(id) => {
                let current = self.find_list_item_by_id(id)?.elem_id;
                if element == current {
                    return Some(current);
                }
                let initial = self.blocks.block_topology.initial_items.get(&id)?;
                (self.blocks.block_topology.item_incarnation(id, element) == Some(initial.id))
                    .then_some(current)
            }
        }
    }

    fn physical_parent(
        &self,
        parent: Option<LogicalContainer>,
        origin: Option<OpId>,
    ) -> Option<Option<OpId>> {
        if let Some(container) = parent
            && self.blocks.block_topology.current_target(container)?.origin != origin?
        {
            return None;
        }
        match parent {
            None => Some(None),
            Some(LogicalContainer::Block(id)) => self.block_elem_id(id).map(Some),
            Some(LogicalContainer::Item(id)) => {
                self.find_list_item_by_id(id).map(|item| Some(item.elem_id))
            }
        }
    }

    pub(crate) fn capture_block_topology(&mut self) {
        collect_metadata(
            &self.blocks.sequence,
            None,
            &mut self.blocks.block_topology,
            true,
        );
    }

    pub(crate) fn topology_state(&self) -> BlockTopology {
        let mut metadata = self.blocks.block_topology.clone();
        collect_metadata(&self.blocks.sequence, None, &mut metadata, true);
        metadata.recompute_deletions();
        metadata
    }

    // Losing proposals must dirty the same source regions as winning proposals.
    fn dirty_placement_sources(&mut self, nodes: impl IntoIterator<Item = LogicalContainer>) {
        let Some(source) = &mut self.source else {
            return;
        };
        let metadata = &self.blocks.block_topology;
        let bound = metadata
            .initial
            .len()
            .saturating_add(metadata.initial_items.len())
            .saturating_add(1);
        for node in nodes {
            let mut current = Some(node);
            for _ in 0..bound {
                let Some(node) = current else {
                    break;
                };
                current = match node {
                    LogicalContainer::Block(id) => {
                        source.mark_root_dirty(id);
                        metadata.initial.get(&id).and_then(|p| {
                            let target = LogicalTarget {
                                container: p.parent?,
                                origin: p.parent_origin?,
                            };
                            (metadata.current_target(target.container) == Some(target))
                                .then_some(target.container)
                        })
                    }
                    LogicalContainer::Item(id) => metadata.initial_items.get(&id).and_then(|p| {
                        let target = LogicalTarget {
                            container: LogicalContainer::Block(p.list_id),
                            origin: p.list_origin,
                        };
                        (metadata.current_target(target.container) == Some(target))
                            .then_some(target.container)
                    }),
                };
            }
        }
    }

    /// Every retained block placement is also a root tombstone anchor. This makes
    /// a later derived root fallback canonical even on peers that never needed it.
    pub(crate) fn initialize_placement_roots(&mut self) {
        let anchors: Vec<_> = self
            .blocks
            .block_topology
            .initial
            .values()
            .map(|p| (p.id, p.after, p.right_origin))
            .chain(
                self.blocks
                    .block_topology
                    .moves
                    .iter()
                    .flat_map(|m| m.blocks.iter().map(|b| (b.id, b.after, b.right_origin))),
            )
            .collect();
        loop {
            let mut progress = false;
            for (id, after, right) in &anchors {
                progress |= self.blocks.insert_tombstone(*after, *id, *right);
            }
            if !progress {
                break;
            }
        }
    }

    pub(crate) fn logical_subtree(&self, root: LogicalContainer) -> Vec<LogicalTarget> {
        fn collect_block(
            value: &super::Block,
            metadata: &BlockTopology,
            result: &mut Vec<LogicalTarget>,
        ) {
            result.push(LogicalTarget {
                container: LogicalContainer::Block(value.id),
                origin: metadata
                    .initial
                    .get(&value.id)
                    .map_or(value.elem_id, |p| p.id),
            });
            match &value.kind {
                super::BlockKind::BlockQuote { children } => {
                    for child in children.iter() {
                        collect_block(child, metadata, result);
                    }
                }
                super::BlockKind::List { items, .. } => {
                    for child in items.iter() {
                        collect_item(child, metadata, result);
                    }
                }
                _ => {}
            }
        }
        fn collect_item(
            value: &super::ListItem,
            metadata: &BlockTopology,
            result: &mut Vec<LogicalTarget>,
        ) {
            result.push(LogicalTarget {
                container: LogicalContainer::Item(value.id),
                origin: metadata
                    .initial_items
                    .get(&value.id)
                    .map_or(value.elem_id, |p| p.id),
            });
            for child in value.children.iter() {
                collect_block(child, metadata, result);
            }
        }
        let mut result = Vec::new();
        match root {
            LogicalContainer::Block(id) => {
                if let Some(value) = self.find_block_by_id(id) {
                    collect_block(value, &self.blocks.block_topology, &mut result);
                }
            }
            LogicalContainer::Item(id) => {
                if let Some(value) = self.find_list_item_by_id(id) {
                    collect_item(value, &self.blocks.block_topology, &mut result);
                }
            }
        }
        result
    }

    pub(crate) fn delete_logical_subtree(
        &mut self,
        id: OpId,
        target: LogicalTarget,
        observed: &StateVector,
        subtree: &[LogicalTarget],
    ) {
        self.capture_block_topology();
        if !self
            .blocks
            .block_topology
            .deletions
            .iter()
            .any(|d| d.id == id)
        {
            self.blocks.block_topology.deletions.push(SubtreeDeletion {
                id,
                target,
                observed: observed.clone(),
                subtree: subtree.to_vec(),
            });
            self.blocks.block_topology.deletions.sort_by_key(|d| d.id);
        }
        self.dirty_placement_sources(
            std::iter::once(target.container).chain(subtree.iter().map(|target| target.container)),
        );
        self.project_block_topology();
    }

    pub(crate) fn record_block_placement(
        &mut self,
        block: &super::Block,
        parent: Option<OpId>,
        id: OpId,
        after: Option<OpId>,
        right_origin: Option<OpId>,
    ) {
        let target = parent.and_then(|element| {
            let container = self.logical_container(element)?;
            self.blocks
                .block_topology
                .reference_target(container, element)
        });
        record_metadata(
            block,
            BlockPlacement {
                parent: target.map(|target| target.container),
                parent_origin: target.map(|target| target.origin),
                id,
                after,
                right_origin,
            },
            &mut self.blocks.block_topology,
            false,
        );
        self.blocks.block_topology.recompute_deletions();
        self.dirty_placement_sources(
            std::iter::once(LogicalContainer::Block(block.id))
                .chain(target.map(|target| target.container)),
        );
    }

    pub(crate) fn record_list_item_creation(
        &mut self,
        list_id: BlockId,
        list_elem: OpId,
        id: OpId,
        after: Option<OpId>,
        right_origin: Option<OpId>,
    ) -> bool {
        let logical = block_id_from_op(id);
        if !self.blocks.block_topology.initial.contains_key(&list_id) {
            self.capture_block_topology();
        }
        let Some(reference) = self
            .blocks
            .block_topology
            .reference_target(LogicalContainer::Block(list_id), list_elem)
        else {
            return false;
        };
        if self
            .blocks
            .block_topology
            .current_target(reference.container)
            == Some(reference)
            && self
                .find_block_by_id(list_id)
                .is_some_and(|block| !matches!(block.kind, super::BlockKind::List { .. }))
        {
            return false;
        }
        let list_origin = reference.origin;
        self.blocks.block_topology.record_item_creation(
            logical,
            ItemPlacement {
                list_id,
                list_origin,
                id,
                after,
                right_origin,
            },
            false,
        );
        record_item_alias(&mut self.blocks.block_topology, id, logical);
        self.blocks.block_topology.recompute_deletions();
        self.dirty_placement_sources([
            LogicalContainer::Item(logical),
            LogicalContainer::Block(list_id),
        ]);
        self.blocks
            .block_topology
            .current_target(reference.container)
            == Some(reference)
            && !self.blocks.block_topology.deleted.contains(&list_id)
            && !self.blocks.block_topology.deleted_items.contains(&logical)
    }

    pub(crate) fn item_placement_register(&self, logical: BlockId) -> Option<ItemPlacementState> {
        if let Some(state) = self
            .blocks
            .block_topology
            .item_register(logical, &BTreeSet::new())
        {
            return Some(state);
        }
        let item = self.find_list_item_by_id(logical)?;
        let (list_id, _, current) = self.list_item_placement(logical)?;
        let element = self.list_items(list_id)?.get_element(&current)?;
        Some(ItemPlacementState {
            value: ListItemPlacement {
                list_id,
                placement: Placement {
                    after: element.after,
                    right_origin: element.right_origin,
                },
            },
            id: current,
            observed: item.placement_observed.clone(),
            conflicts: item.placement_conflicts.clone(),
            implicit: block_id_from_op(current) == logical,
        })
    }

    pub(crate) fn apply_item_movement(&mut self, movement: &PendingListItemMove) -> bool {
        let destination = LogicalContainer::Block(movement.list_id);
        let referenced = self
            .blocks
            .block_topology
            .reference_target(destination, movement.to_list_elem);
        if referenced == self.blocks.block_topology.current_target(destination)
            && self
                .find_block_by_id(movement.list_id)
                .is_some_and(|block| !matches!(block.kind, super::BlockKind::List { .. }))
        {
            return false;
        }
        self.capture_block_topology();
        if self
            .blocks
            .block_topology
            .item_moves
            .iter()
            .any(|m| m.id == movement.id)
        {
            return false;
        }
        if !self
            .blocks
            .block_topology
            .initial_items
            .contains_key(&movement.item_id)
        {
            let source = self
                .logical_block_for_elem(movement.from_list_elem)
                .and_then(|id| self.block_elem_id(id));
            return source
                .and_then(|source| {
                    self.with_block_mut(source, |block| {
                        if let super::BlockKind::List { pending_moves, .. } = &mut block.kind {
                            if !pending_moves
                                .iter()
                                .any(|pending| pending.id == movement.id)
                            {
                                pending_moves.push(movement.clone());
                            }
                            true
                        } else {
                            false
                        }
                    })
                })
                .unwrap_or(false);
        }
        record_item_alias(
            &mut self.blocks.block_topology,
            movement.id,
            movement.item_id,
        );
        self.blocks.block_topology.item_moves.push(movement.clone());
        self.blocks.block_topology.item_moves.sort_by_key(|m| m.id);
        let source = self.logical_block_for_elem(movement.from_list_elem);
        self.dirty_placement_sources(
            [
                LogicalContainer::Item(movement.item_id),
                LogicalContainer::Block(movement.list_id),
            ]
            .into_iter()
            .chain(source.map(LogicalContainer::Block)),
        );
        self.project_block_topology();
        true
    }

    /// Transfer changed bodies by ownership; never clone a subtree into a placement.
    pub(crate) fn move_blocks_at(
        &mut self,
        to_parent: Option<OpId>,
        blocks: &[MovedBlockWire],
        id: OpId,
        observed: &StateVector,
    ) -> bool {
        self.capture_block_topology();
        let parent = match to_parent {
            Some(element) => match self.logical_container(element) {
                Some(parent) => Some(parent),
                None => return false,
            },
            None => None,
        };
        if blocks.is_empty() || self.blocks.block_topology.moves.iter().any(|m| m.id == id) {
            return false;
        }
        let parent_origin = to_parent.and_then(|element| {
            self.blocks
                .block_topology
                .reference_target(parent?, element)
                .map(|target| target.origin)
        });
        if let Some(container) = parent {
            let reference = parent_origin.map(|origin| LogicalTarget { container, origin });
            if reference == self.blocks.block_topology.current_target(container)
                && to_parent.is_some_and(|element| self.resolve_container_elem(element).is_some())
                && self.container_children(to_parent).is_none()
            {
                return false;
            }
        }
        self.blocks.block_topology.moves.push(BlockMove {
            id,
            parent,
            parent_origin,
            blocks: blocks.to_vec(),
            observed: observed.clone(),
        });
        self.blocks.block_topology.moves.sort_by_key(|m| m.id);
        self.dirty_placement_sources(
            blocks
                .iter()
                .map(|block| LogicalContainer::Block(block.block_id))
                .chain(parent),
        );
        self.project_block_topology();
        true
    }

    pub(crate) fn project_block_topology(&mut self) {
        self.blocks.block_topology.recompute_deletions();
        fn sequence_at_path<'a>(
            sequence: &'a mut crate::core::Sequence<super::Block>,
            containers: &[super::BlockContainerPath],
        ) -> Option<&'a mut crate::core::Sequence<super::Block>> {
            let Some((container, rest)) = containers.split_first() else {
                return Some(sequence);
            };
            match *container {
                super::BlockContainerPath::BlockQuote(id) => {
                    let super::BlockKind::BlockQuote { children } =
                        &mut sequence.value_mut(id)?.kind
                    else {
                        return None;
                    };
                    sequence_at_path(children, rest)
                }
                super::BlockContainerPath::ListItem { list, item } => {
                    let super::BlockKind::List { items, .. } = &mut sequence.value_mut(list)?.kind
                    else {
                        return None;
                    };
                    sequence_at_path(&mut items.value_mut(item)?.children, rest)
                }
            }
        }
        enum Detach {
            Block(BlockId, super::BlockPath),
            Item(BlockId, super::BlockPath, OpId),
        }
        let selected = self.blocks.block_topology.project();
        self.ensure_block_index();
        let mut changed = {
            let cached = self.block_index_read();
            let index = &cached.as_ref().expect("index installed").index;
            let mut changed = Vec::new();
            for (logical, path) in &index.by_block_id {
                if self.blocks.block_topology.deleted.contains(logical)
                    || selected.blocks.get(logical).is_some_and(|p| {
                        let parent =
                            path.containers
                                .last()
                                .and_then(|container| match *container {
                                    super::BlockContainerPath::BlockQuote(id) => {
                                        self.logical_container(id)
                                    }
                                    super::BlockContainerPath::ListItem { item, .. } => {
                                        self.logical_container(item)
                                    }
                                });
                        p.id != path.elem_id || p.parent != parent
                    })
                {
                    changed.push((path.containers.len(), Detach::Block(*logical, path.clone())));
                }
            }
            for (logical, (list, item)) in &index.by_item_id {
                if self.blocks.block_topology.deleted_items.contains(logical)
                    || selected.items.get(logical).is_some_and(|p| {
                        p.id != *item || self.logical_block_for_elem(*list) != Some(p.list_id)
                    })
                {
                    let path = index.by_elem_id[list].clone();
                    changed.push((
                        path.containers.len() + 1,
                        Detach::Item(*logical, path, *item),
                    ));
                }
            }
            changed
        };
        for (_, change) in &changed {
            let elem = match change {
                Detach::Block(_, path) | Detach::Item(_, path, _) => path.elem_id,
            };
            self.mark_source_elem_dirty(elem);
        }
        changed.sort_by_key(|(depth, _)| std::cmp::Reverse(*depth));
        let mut blocks = BTreeMap::new();
        let mut items = BTreeMap::new();
        for (_, change) in changed {
            match change {
                Detach::Block(logical, path) => {
                    if let Some(block) = sequence_at_path(&mut self.blocks, &path.containers)
                        .and_then(|s| s.take_value(path.elem_id))
                    {
                        blocks.insert(logical, block);
                    }
                }
                Detach::Item(logical, path, item) => {
                    if let Some(block) =
                        super::block_at_path_mut(&mut self.blocks, &path.containers, path.elem_id)
                        && let super::BlockKind::List {
                            items: sequence, ..
                        } = &mut block.kind
                        && let Some(item) = sequence.take_value(item)
                    {
                        items.insert(logical, item);
                    }
                }
            }
        }
        let history: Vec<_> = self
            .blocks
            .block_topology
            .initial
            .values()
            .copied()
            .chain(self.blocks.block_topology.moves.iter().flat_map(|m| {
                m.blocks.iter().map(|b| BlockPlacement {
                    parent_origin: m.parent_origin,
                    parent: m.parent,
                    id: b.id,
                    after: b.after,
                    right_origin: b.right_origin,
                })
            }))
            .collect();
        let item_history: Vec<_> = self
            .blocks
            .block_topology
            .initial_items
            .values()
            .copied()
            .chain(
                self.blocks
                    .block_topology
                    .item_moves
                    .iter()
                    .filter_map(|m| {
                        let list_origin = self
                            .blocks
                            .block_topology
                            .reference_target(LogicalContainer::Block(m.list_id), m.to_list_elem)?
                            .origin;
                        Some(ItemPlacement {
                            list_id: m.list_id,
                            list_origin,
                            id: m.id,
                            after: m.after,
                            right_origin: m.right_origin,
                        })
                    }),
            )
            .collect();
        self.initialize_placement_roots();
        loop {
            let mut progress = false;
            for placement in &history {
                let Some(parent) = self.physical_parent(placement.parent, placement.parent_origin)
                else {
                    continue;
                };
                progress |= self
                    .with_children_at(parent, |s| {
                        s.insert_tombstone(placement.after, placement.id, placement.right_origin)
                    })
                    .unwrap_or(false);
            }
            for placement in &item_history {
                if self
                    .blocks
                    .block_topology
                    .current_target(LogicalContainer::Block(placement.list_id))
                    .is_none_or(|target| target.origin != placement.list_origin)
                {
                    continue;
                }
                if !self.list_items(placement.list_id).is_some_and(|s| {
                    s.get_element(&placement.id).is_none()
                        && placement
                            .after
                            .is_none_or(|anchor| s.get_element(&anchor).is_some())
                }) {
                    continue;
                }
                progress |= self
                    .with_items_at(placement.list_id, |s| {
                        s.insert_tombstone(placement.after, placement.id, placement.right_origin)
                    })
                    .unwrap_or(false);
            }
            let ready: Vec<_> = blocks
                .keys()
                .copied()
                .filter(|logical| {
                    !self.blocks.block_topology.deleted.contains(logical)
                        && self
                            .physical_parent(
                                selected.blocks[logical].parent,
                                selected.blocks[logical].parent_origin,
                            )
                            .is_some_and(|parent| {
                                self.container_children(parent).is_some_and(|s| {
                                    s.get_element(&selected.blocks[logical].id).is_some()
                                })
                            })
                })
                .collect();
            for logical in ready {
                let placement = selected.blocks[&logical];
                let parent = self
                    .physical_parent(placement.parent, placement.parent_origin)
                    .expect("ready parent");
                let mut block = blocks.remove(&logical).expect("detached block");
                block.elem_id = placement.id;
                self.with_children_at(parent, |s| s.update_value(placement.id, block));
                self.mark_source_elem_dirty(placement.id);
                progress = true;
            }
            let ready: Vec<_> = items
                .keys()
                .copied()
                .filter(|logical| {
                    !self.blocks.block_topology.deleted_items.contains(logical)
                        && self
                            .blocks
                            .block_topology
                            .current_target(LogicalContainer::Block(
                                selected.items[logical].list_id,
                            ))
                            .is_some_and(|target| {
                                target.origin == selected.items[logical].list_origin
                            })
                        && self
                            .list_items(selected.items[logical].list_id)
                            .is_some_and(|s| s.get_element(&selected.items[logical].id).is_some())
                })
                .collect();
            for logical in ready {
                let placement = selected.items[&logical];
                let mut item = items.remove(&logical).expect("detached item");
                item.elem_id = placement.id;
                self.with_items_at(placement.list_id, |s| s.update_value(placement.id, item));
                progress = true;
            }
            if !progress {
                break;
            }
        }
        assert!(
            blocks
                .keys()
                .all(|id| self.blocks.block_topology.deleted.contains(id)),
            "surviving block lost during placement publication"
        );
        assert!(
            items
                .keys()
                .all(|id| self.blocks.block_topology.deleted_items.contains(id)),
            "surviving item lost during placement publication"
        );
        let states: Vec<_> = selected
            .items
            .iter()
            .filter_map(|(logical, placement)| {
                self.item_placement_register(*logical)
                    .map(|state| (*logical, *placement, state))
            })
            .collect();
        for (logical, placement, state) in states {
            if self.find_list_item_by_id(logical).is_none_or(|item| {
                item.placement_observed == state.observed
                    && item.placement_conflicts == state.conflicts
            }) {
                continue;
            }
            self.with_items_at(placement.list_id, |items| {
                if let Some(item) = items.value_mut(placement.id)
                    && item.id == logical
                {
                    item.placement_observed = state.observed;
                    item.placement_conflicts = state.conflicts;
                }
            });
        }
    }

    fn with_items_at<R>(
        &mut self,
        logical: BlockId,
        f: impl FnOnce(&mut crate::core::Sequence<super::ListItem>) -> R,
    ) -> Option<R> {
        let element = self.block_elem_id(logical)?;
        self.with_block_mut(element, |block| {
            let super::BlockKind::List { items, .. } = &mut block.kind else {
                return None;
            };
            Some(f(items))
        })
        .flatten()
    }

    fn with_children_at<R>(
        &mut self,
        parent: Option<OpId>,
        f: impl FnOnce(&mut crate::core::Sequence<super::Block>) -> R,
    ) -> Option<R> {
        match parent {
            None => Some(f(&mut self.blocks)),
            Some(parent) => self.with_container_children_mut(parent, f),
        }
    }
}

fn record_item_alias(topology: &mut BlockTopology, element: OpId, logical: BlockId) {
    if let Err(index) = topology
        .item_aliases
        .binary_search_by_key(&element, |(id, _)| *id)
    {
        topology.item_aliases.insert(index, (element, logical));
    }
}

fn collect_metadata(
    sequence: &crate::core::Sequence<super::Block>,
    parent: Option<LogicalContainer>,
    topology: &mut BlockTopology,
    imported: bool,
) {
    for element in sequence.iter_all() {
        if let Some(block) = &element.value {
            let parent_origin = parent
                .and_then(|parent| topology.current_target(parent))
                .map(|target| target.origin);
            record_metadata(
                block,
                BlockPlacement {
                    parent,
                    parent_origin,
                    id: element.id,
                    after: element.after,
                    right_origin: element.right_origin,
                },
                topology,
                imported,
            );
        }
    }
}

fn record_metadata(
    block: &super::Block,
    placement: BlockPlacement,
    topology: &mut BlockTopology,
    imported: bool,
) {
    let replace = topology.deleted.contains(&block.id)
        && topology
            .initial
            .get(&block.id)
            .is_none_or(|old| old.id != placement.id);
    let newly_registered = !topology.initial.contains_key(&block.id) || replace;
    topology.record_creation(block.id, placement, replace);
    if imported && newly_registered {
        topology.imported_origins.insert(LogicalTarget {
            container: LogicalContainer::Block(block.id),
            origin: placement.id,
        });
    }
    match &block.kind {
        super::BlockKind::BlockQuote { children } => collect_metadata(
            children,
            Some(LogicalContainer::Block(block.id)),
            topology,
            imported,
        ),
        super::BlockKind::List { items, .. } => {
            for element in items.iter_all() {
                let Some(item) = &element.value else {
                    continue;
                };
                record_item_alias(topology, item.elem_id, item.id);
                let replace = topology.deleted_items.contains(&item.id)
                    && topology
                        .initial_items
                        .get(&item.id)
                        .is_none_or(|old| old.id != element.id);
                let newly_registered = !topology.initial_items.contains_key(&item.id) || replace;
                let list_origin = topology.initial[&block.id].id;
                topology.record_item_creation(
                    item.id,
                    ItemPlacement {
                        list_id: block.id,
                        list_origin,
                        id: element.id,
                        after: element.after,
                        right_origin: element.right_origin,
                    },
                    replace,
                );
                if imported && newly_registered {
                    topology.imported_origins.insert(LogicalTarget {
                        container: LogicalContainer::Item(item.id),
                        origin: element.id,
                    });
                }
                collect_metadata(
                    &item.children,
                    Some(LogicalContainer::Item(item.id)),
                    topology,
                    imported,
                );
            }
        }
        _ => {}
    }
}
