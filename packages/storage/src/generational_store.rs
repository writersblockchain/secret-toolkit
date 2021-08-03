//! A "generational index store" is a storage wrapper for a generational index, which allows for constant time insert and 
//! removal of arbitrary entries in a list. In this case an appendstore will not be suitable because it will mess up the indexes
//! of all entries that follow the one that was deleted. Each get from the store requires a tuple of (index, generation), where
//! generation is a monotonically increasing value that records the generation of the current data value at that index.
//! 
//! Generational indexes are commonly used in Entity Component System architectures for game development, but can be used in
//! other situations. It is a useful data structure for adding or deleting nodes from a graph in constant time; for example, 
//! if you are building a social network application and want to record follower relationships.
//! 
//! Unlike an appendstore, the order of iteration over the entries is not specified.
//!
//! The implementation was inspired by the [generational arena repository](https://github.com/fitzgen/generational-arena), 
//! which in turn was inspired by [Catherine West's closing keynote at RustConf 2018](https://www.youtube.com/watch?v=aKLntZcp27M).
//! 

use std::convert::TryInto;
use std::marker::PhantomData;

use serde::{de::DeserializeOwned, Serialize, Deserialize};

use cosmwasm_std::{ReadonlyStorage, StdError, StdResult, Storage};

use secret_toolkit_serialization::{Bincode2, Serde};

const LEN_KEY: &[u8] = b"len";
const GENERATION_KEY: &[u8] = b"gen";
const FREE_LIST_HEAD_KEY: &[u8] = b"head";
const CAPACITY_KEY: &[u8] = b"cap";

#[derive(Serialize, Clone, Debug, PartialEq, Eq, PartialOrd)]
pub struct Index {
    index: u32,
    generation: u64,
}

impl Index {
    /// Create a new `Index` from its raw parts.
    ///
    /// The parts must have been returned from an earlier call to
    /// `into_raw_parts`.
    ///
    /// Providing arbitrary values will lead to malformed indices and ultimately
    /// panics.
    pub fn from_raw_parts(a: u32, b: u64) -> Index {
        Index {
            index: a,
            generation: b,
        }
    }

    /// Convert this `Index` into its raw parts.
    ///
    /// This niche method is useful for converting an `Index` into another
    /// identifier type. Usually, you should prefer a newtype wrapper around
    /// `Index` like `pub struct MyIdentifier(Index);`.  However, for external
    /// types whose definition you can't customize, but which you can construct
    /// instances of, this method can be useful.
    pub fn into_raw_parts(self) -> (u32, u64) {
        (self.index, self.generation)
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub enum Entry<T> {
    Free { next_free: u32 },
    Occupied { generation: u64, value: T },
}

// Mutable generational index store

/// A type allowing both reads from and writes to the generational store.
#[derive(Debug)]
pub struct GenerationalStoreMut<'a, T, S, Ser = Bincode2>
where
    T: Serialize + DeserializeOwned,
    S: Storage,
    Ser: Serde,
{
    storage: &'a mut S,
    item_type: PhantomData<*const T>,
    serialization_type: PhantomData<*const Ser>,
    len: u32,
    generation: u64,
    free_list_head: u32,
    // used for iterator
    capacity: u32,
}

impl<'a, T, S> GenerationalStoreMut<'a, T, S, Bincode2>
where
    T: Serialize + DeserializeOwned,
    S: Storage,
{
    /// Try to use the provided storage as an GenerationalStore. If it doesn't seem to be one, then
    /// initialize it as one.
    ///
    /// Returns Err if the contents of the storage can not be parsed.
    pub fn attach_or_create(storage: &'a mut S) -> StdResult<Self> {
        GenerationalStoreMut::attach_or_create_with_serialization(storage, Bincode2)
    }

    /// Try to use the provided storage as an GenerationalStore.
    ///
    /// Returns None if the provided storage doesn't seem like an GenerationalStore.
    /// Returns Err if the contents of the storage can not be parsed.
    pub fn attach(storage: &'a mut S) -> Option<StdResult<Self>> {
        GenerationalStoreMut::attach_with_serialization(storage, Bincode2)
    }
}

impl<'a, T, S, Ser> GenerationalStoreMut<'a, T, S, Ser>
where
    T: Serialize + DeserializeOwned,
    S: Storage,
    Ser: Serde,
{
    /// Try to use the provided storage as an GenerationalStore. If it doesn't seem to be one, then
    /// initialize it as one. This method allows choosing the serialization format you want to use.
    ///
    /// Returns Err if the contents of the storage can not be parsed.
    pub fn attach_or_create_with_serialization(storage: &'a mut S, _ser: Ser) -> StdResult<Self> {
        let len_vec_wrapped = storage.get(LEN_KEY);
        let generation_vec_wrapped = storage.get(GENERATION_KEY);
        let free_list_head_vec_wrapped = storage.get(FREE_LIST_HEAD_KEY);
        let capacity_vec_wrapped = storage.get(CAPACITY_KEY);
        if len_vec_wrapped.is_some() && generation_vec_wrapped.is_some() && 
           free_list_head_vec_wrapped.is_some() && capacity_vec_wrapped.is_some() {
            Self::new(
                storage, 
                &len_vec_wrapped.unwrap(), 
                &generation_vec_wrapped.unwrap(), 
                &free_list_head_vec_wrapped.unwrap(),
                &capacity_vec_wrapped.unwrap(),
            )
        } else {
            let len_vec = 0_u32.to_be_bytes();
            storage.set(LEN_KEY, &len_vec);
            let generation_vec = 0_u64.to_be_bytes();
            storage.set(GENERATION_KEY, &generation_vec);
            let free_list_head_vec = 0_u32.to_be_bytes();
            storage.set(FREE_LIST_HEAD_KEY, &free_list_head_vec);
            let capacity_vec = 0_u32.to_be_bytes();
            storage.set(CAPACITY_KEY, &capacity_vec);

            // initialize first empty
            //let entry: Entry<()> = Entry::Free {
            //    next_free: 1_u32,
            //};
            //let serialized = Ser::serialize(&entry)?;
            //storage.set(&0_u32.to_be_bytes(), &serialized);

            Self::new(storage, &len_vec, &generation_vec, &free_list_head_vec, &capacity_vec)
        }
    }

    /// Try to use the provided storage as an GenerationalStore.
    /// This method allows choosing the serialization format you want to use.
    ///
    /// Returns None if the provided storage doesn't seem like an AppendStore.
    /// Returns Err if the contents of the storage can not be parsed.
    pub fn attach_with_serialization(storage: &'a mut S, _ser: Ser) -> Option<StdResult<Self>> {
        let len_vec = storage.get(LEN_KEY)?;
        let generation_vec = storage.get(GENERATION_KEY)?;
        let free_list_head_vec = storage.get(FREE_LIST_HEAD_KEY)?;
        let capacity_vec = storage.get(CAPACITY_KEY)?;
        Some(Self::new(storage, &len_vec, &generation_vec, &free_list_head_vec, &capacity_vec))
    }

    fn new(
        storage: &'a mut S, 
        len_vec: &[u8], 
        generation_vec: &[u8], 
        free_list_head_vec: &[u8],
        capacity_vec: &[u8],
    ) -> StdResult<Self> {
        let len_array = len_vec
            .try_into()
            .map_err(|err| StdError::parse_err("u32", err))?;
        let len = u32::from_be_bytes(len_array);

        let generation_array = generation_vec
            .try_into()
            .map_err(|err| StdError::parse_err("u64", err))?;
        let generation = u64::from_be_bytes(generation_array);

        let free_list_head_array = free_list_head_vec
            .try_into()
            .map_err(|err| StdError::parse_err("u32", err))?;
        let free_list_head = u32::from_be_bytes(free_list_head_array);

        let capacity_array = capacity_vec
            .try_into()
            .map_err(|err| StdError::parse_err("u32", err))?;
        let capacity = u32::from_be_bytes(capacity_array);

        Ok(Self {
            storage,
            item_type: PhantomData,
            serialization_type: PhantomData,
            len,
            generation,
            free_list_head,
            capacity,
        })
    }

    pub fn insert(&mut self, value: T) -> Index {
        match self.try_insert(value) {
            Ok(i) => i,
            Err(value) => {
                // add new to end
                self.insert_slow_path(value).ok().unwrap()
            },
        }
    }

    pub fn try_insert(&mut self, value: T) -> Result<Index, T> {
        match self.try_alloc_next_index() {
            None => Err(value),
            Some(index) => {
                let new_entry: Entry<T> = Entry::Occupied {
                    generation: self.generation,
                    value,
                };
                let result = self.set_at_unchecked(index.index, &new_entry);
                match result {
                    Ok(_) => {
                        if index.index >= self.capacity { 
                            // for iter
                            self.set_capacity(index.index + 1);
                        }
                        Ok(index)
                    },
                    Err(_) => { panic!("error serializing new entry in generational index store") }
                }

            },
        }
    }

    fn try_alloc_next_index(&mut self) -> Option<Index> {
        let i = self.as_readonly().get_at_unchecked(self.free_list_head);
        let old_free_list_head = self.free_list_head;
        match i {
            Ok(i) => {
                match i {
                    Entry::Occupied { .. } => panic!("corrupt free list"),
                    Entry::Free { next_free } => {
                        self.set_free_list_head(next_free);
                        self.set_length(self.len + 1);
                        Some(Index {
                            index: old_free_list_head,
                            generation: self.generation,
                        })
                    }
                }
            },
            _ => None,
        }
    }

    fn insert_slow_path(&mut self, value: T) -> StdResult<Index> {
        let start = self.capacity;
        // initialize next empty
        let entry: Entry<()> = Entry::Free {
            next_free: self.free_list_head,
        };
        let serialized = Ser::serialize(&entry)?;
        self.storage.set(&start.to_be_bytes(), &serialized);
        let index = self.try_insert(value)
            .map_err(|_| ())
            .expect("inserting should always succeed");
        self.set_free_list_head(self.capacity);
        Ok(index)
    }

    // removes the entry at a given index
    pub fn remove(&mut self, i: Index) -> StdResult<Option<T>> {
        match self.get_at_unchecked(i.index) {
            Ok(entry) => match entry {
                Entry::Occupied { generation, .. } if i.generation == generation => {
                    let value = self.get(i.clone());
                    self.set_at_unchecked(i.index, &Entry::Free { next_free: self.free_list_head })?;
                    self.set_generation(self.generation + 1);
                    self.set_free_list_head(i.index);
                    self.set_length(self.len - 1);
                    Ok(value)
                }
                _ => Ok(None),
            },
            _ => Ok(None),
        }
    }

    pub fn len(&self) -> u32 {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn storage(&mut self) -> &mut S {
        self.storage
    }

    pub fn readonly_storage(&self) -> &S {
        self.storage
    }

    /// Return an iterator over the items in the collection
    pub fn iter(&self) -> Iter<T, S, Ser> {
        self.as_readonly().iter()
    }

    /// Get the value stored at a given index.
    pub fn get(&self, index: Index) -> Option<T> {
        self.as_readonly().get(index)
    }

    pub fn get_at(&self, pos: u32) -> StdResult<Entry<T>> {
        self.as_readonly().get_at(pos)
    }

    fn get_at_unchecked(&self, pos: u32) -> StdResult<Entry<T>> {
        self.as_readonly().get_at_unchecked(pos)
    }

    pub fn contains(&self, i: Index) -> bool {
        self.get(i).is_some()
    }

    /// Set the value of the item stored at a given position.
    ///
    /// # Errors
    /// Will return an error if the position is out of bounds

    fn set_at_unchecked(&mut self, pos: u32, item: &Entry<T>) -> StdResult<()> {
        let serialized = Ser::serialize(item)?;
        self.storage.set(&pos.to_be_bytes(), &serialized);
        Ok(())
    }

    /// Set the length of the generational index
    fn set_length(&mut self, len: u32) {
        self.storage.set(LEN_KEY, &len.to_be_bytes());
        self.len = len;
    }

    /// Set the free list head of the generational index
    fn set_free_list_head(&mut self, free_list_head: u32) {
        self.storage.set(FREE_LIST_HEAD_KEY, &free_list_head.to_be_bytes());
        self.free_list_head = free_list_head;
    }

    // Set the generation of the generational index
    fn set_generation(&mut self, generation: u64) {
        self.storage.set(GENERATION_KEY, &generation.to_be_bytes());
        self.generation = generation;
    }

    // Set the maximum internal index (for iter)
    fn set_capacity(&mut self, capacity: u32) {
        self.storage.set(CAPACITY_KEY, &capacity.to_be_bytes());
        self.capacity = capacity;
    }

    /// Gain access to the implementation of the immutable methods
    fn as_readonly(&self) -> GenerationalStore<T, S, Ser> {
        GenerationalStore {
            storage: self.storage,
            item_type: self.item_type,
            serialization_type: self.serialization_type,
            len: self.len,
            generation: self.generation,
            free_list_head: self.free_list_head,
            capacity: self.capacity,
        }
    }
}

// Readonly generational index store

/// A type allowing only reads from an append store. useful in the context_, u8 of queries.
#[derive(Debug)]
pub struct GenerationalStore<'a, T, S, Ser = Bincode2>
where
    T: Serialize + DeserializeOwned,
    S: ReadonlyStorage,
    Ser: Serde,
{
    storage: &'a S,
    item_type: PhantomData<*const T>,
    serialization_type: PhantomData<*const Ser>,
    len: u32,
    generation: u64,
    free_list_head: u32,
    capacity: u32,
}

impl<'a, T, S> GenerationalStore<'a, T, S, Bincode2>
where
    T: Serialize + DeserializeOwned,
    S: ReadonlyStorage,
{
    /// Try to use the provided storage as an AppendStore.
    ///
    /// Returns None if the provided storage doesn't seem like an AppendStore.
    /// Returns Err if the contents of the storage can not be parsed.
    pub fn attach(storage: &'a S) -> Option<StdResult<Self>> {
        GenerationalStore::attach_with_serialization(storage, Bincode2)
    }
}

impl<'a, T, S, Ser> GenerationalStore<'a, T, S, Ser>
where
    T: Serialize + DeserializeOwned,
    S: ReadonlyStorage,
    Ser: Serde,
{
    /// Try to use the provided storage as an GenerationalStore.
    /// This method allows choosing the serialization format you want to use.
    ///
    /// Returns None if the provided storage doesn't seem like an AppendStore.
    /// Returns Err if the contents of the storage can not be parsed.
    pub fn attach_with_serialization(storage: &'a S, _ser: Ser) -> Option<StdResult<Self>> {
        let len_vec = storage.get(LEN_KEY)?;
        let generation_vec = storage.get(GENERATION_KEY)?;
        let free_list_head_vec = storage.get(FREE_LIST_HEAD_KEY)?;
        let capacity_vec = storage.get(CAPACITY_KEY)?;
        Some(GenerationalStore::new(storage, len_vec, generation_vec, free_list_head_vec, capacity_vec))
    }

    fn new(
        storage: &'a S, 
        len_vec: Vec<u8>, 
        generation_vec: Vec<u8>,
        free_list_head_vec: Vec<u8>,
        capacity_vec: Vec<u8>,
    ) -> StdResult<Self> {
        let len_array = len_vec
            .as_slice()
            .try_into()
            .map_err(|err| StdError::parse_err("u32", err))?;
        let len = u32::from_be_bytes(len_array);

        let generation_array = generation_vec
            .as_slice()
            .try_into()
            .map_err(|err| StdError::parse_err("u64", err))?;
        let generation = u64::from_be_bytes(generation_array);

        let free_list_head_array = free_list_head_vec
            .as_slice()
            .try_into()
            .map_err(|err| StdError::parse_err("u32", err))?;
        let free_list_head = u32::from_be_bytes(free_list_head_array);

        let capacity_array = capacity_vec
            .as_slice()
            .try_into()
            .map_err(|err| StdError::parse_err("u32", err))?;
        let capacity = u32::from_be_bytes(capacity_array);

        Ok(Self {
            storage,
            item_type: PhantomData,
            serialization_type: PhantomData,
            len,
            generation,
            free_list_head,
            capacity,
        })
    }

    pub fn len(&self) -> u32 {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn capacity(&self) -> u32 {
        self.capacity
    }

    pub fn readonly_storage(&self) -> &S {
        self.storage
    }

    /// Return an iterator over the items in the collection
    pub fn iter(&self) -> Iter<'a, T, S, Ser> {
        Iter {
            storage: GenerationalStore::clone(self),
            start: 0,
            end: self.capacity,
        }
    }

    /// Get the value stored at a given position.
    pub fn get_at(&self, pos: u32) -> StdResult<Entry<T>> {
        self.get_at_unchecked(pos)
    }

    fn get_at_unchecked(&self, pos: u32) -> StdResult<Entry<T>> {
        let serialized = self.storage.get(&pos.to_be_bytes()).ok_or_else(|| {
            StdError::generic_err(format!("No item in GenerationalStorage at position {}", pos))
        })?;
        Ser::deserialize(&serialized)
    }

    pub fn get(&self, i: Index) -> Option<T> {
        let item = self.get_at_unchecked(i.index);
        match item {
            Ok(item) => {
                match item {
                    Entry::Occupied {
                        generation,
                        value,
                    } if generation == i.generation => Some(value),
                    _ => None,
                }
            },
            _ => None,
        }
    }

    pub fn contains(&self, i: Index) -> bool {
        self.get(i).is_some()
    }
}

impl<'a, T, S, Ser> IntoIterator for GenerationalStore<'a, T, S, Ser>
where
    T: Serialize + DeserializeOwned,
    S: ReadonlyStorage,
    Ser: Serde,
{
    type Item = (Option<Index>, Entry<T>);

    // alternate version, see below
    //type Item = (Index, T);

    type IntoIter = Iter<'a, T, S, Ser>;

    fn into_iter(self) -> Iter<'a, T, S, Ser> {
        let end = self.len;
        Iter {
            storage: self,
            start: 0,
            end,
        }
    }
}

// Manual `Clone` implementation because the default one tries to clone the Storage??
impl<'a, T, S, Ser> Clone for GenerationalStore<'a, T, S, Ser>
where
    T: Serialize + DeserializeOwned,
    S: ReadonlyStorage,
    Ser: Serde,
{
    fn clone(&self) -> Self {
        Self {
            storage: &self.storage,
            item_type: self.item_type,
            serialization_type: self.serialization_type,
            len: self.len,
            generation: self.generation,
            free_list_head: self.free_list_head,
            capacity: self.capacity,
        }
    }
}

// Owning iterator

/// An iterator over the contents of the generational store.
#[derive(Debug)]
pub struct Iter<'a, T, S, Ser>
where
    T: Serialize + DeserializeOwned,
    S: ReadonlyStorage,
    Ser: Serde,
{
    storage: GenerationalStore<'a, T, S, Ser>,
    start: u32,
    end: u32,
}

impl<'a, T, S, Ser> Iterator for Iter<'a, T, S, Ser>
where
    T: Serialize + DeserializeOwned,
    S: ReadonlyStorage,
    Ser: Serde,
{
    type Item = (Option<Index>, Entry<T>);

    fn next(&mut self) -> Option<Self::Item> {
        if self.start >= self.end {
            return None;
        }
        let item = self.storage.get_at(self.start);
        match item {
            Ok(entry) => {
                match entry {
                    Entry::Free { .. } => {
                        self.start += 1;
                        Some((
                            None,
                            entry
                        ))
                    },
                    Entry::Occupied {generation, ..} => {
                        let index = Index {
                            index: self.start,
                            generation,
                        };
                        self.start += 1;
                        return Some((Some(index), entry))
                    }
                }
            },
            Err(_) => None,
        }
    }

    /* alternative version - automatically filters Free entries
    type Item = (Index, T);

    fn next(&mut self) -> Option<Self::Item> {
        if self.start >= self.end {
            return None;
        }
        let mut hop = 0_u32;
        loop {
            if (self.start + hop) >= self.end { break; }
            match self.storage.get_at(self.start + hop) {
                Ok(entry) => {
                    match entry {
                        Entry::Free { .. } => {
                            hop += 1;
                            continue;
                        },
                        Entry::Occupied {generation, value} => {
                            let index = Index {
                                index: self.start + hop,
                                generation,
                            };
                            self.start += hop + 1;
                            return Some((index, value))
                        }
                    }
                }
                Err(_) => { // shouldn't happen
                    hop += 1;
                    continue;
                },
            }
        }

        self.start += hop + 1;
        None
    }
    */

    // This needs to be implemented correctly for `ExactSizeIterator` to work.
    fn size_hint(&self) -> (usize, Option<usize>) {
        let len = (self.end - self.start) as usize;
        (len, Some(len))
    }

    // I implement `nth` manually because it is used in the standard library whenever
    // it wants to skip over elements, but the default implementation repeatedly calls next.
    // because that is very expensive in this case, and the items are just discarded, we can
    // do better here.
    // In practice, this enables cheap paging over the storage by calling:
    // `append_store.iter().skip(start).take(length).collect()`
    fn nth(&mut self, n: usize) -> Option<Self::Item> {
        self.start = self.start.saturating_add(n as u32);
        self.next()
    }
}

impl<'a, T, S, Ser> DoubleEndedIterator for Iter<'a, T, S, Ser>
where
    T: Serialize + DeserializeOwned,
    S: ReadonlyStorage,
    Ser: Serde,
{
    fn next_back(&mut self) -> Option<Self::Item> {
        if self.start >= self.end {
            return None;
        }
        self.end -= 1;
        let item = self.storage.get_at(self.end);
        match item {
            Ok(entry) => {
                match entry {
                    Entry::Free { .. } => {
                        Some((
                            None,
                            entry
                        ))
                    },
                    Entry::Occupied {generation, ..} => {
                        let index = Index {
                            index: self.start,
                            generation,
                        };
                        Some((Some(index), entry))
                    }
                }
            },
            Err(_) => None,
        }
    }

    // I implement `nth_back` manually because it is used in the standard library whenever
    // it wants to skip over elements, but the default implementation repeatedly calls next_back.
    // because that is very expensive in this case, and the items are just discarded, we can
    // do better here.
    // In practice, this enables cheap paging over the storage by calling:
    // `append_store.iter().skip(start).take(length).collect()`
    fn nth_back(&mut self, n: usize) -> Option<Self::Item> {
        self.end = self.end.saturating_sub(n as u32);
        self.next_back()
    }
}

// This enables writing `append_store.iter().skip(n).rev()`
impl<'a, T, S, Ser> ExactSizeIterator for Iter<'a, T, S, Ser>
where
    T: Serialize + DeserializeOwned,
    S: ReadonlyStorage,
    Ser: Serde,
{
}

#[cfg(test)]
mod tests {
    use cosmwasm_std::testing::MockStorage;

    use secret_toolkit_serialization::Json;

    use super::*;

    #[test]
    fn test_insert_get() -> StdResult<()> {
        let mut storage = MockStorage::new();
        let mut gen_store = GenerationalStoreMut::attach_or_create(&mut storage)?;
        let alpha = gen_store.insert(String::from("Alpha"));
        let beta = gen_store.insert(String::from("Beta"));
        let gamma = gen_store.insert(String::from("Gamma"));
        let delta = gen_store.insert(String::from("Delta"));

        assert_eq!(gen_store.get(alpha), Some(String::from("Alpha")));
        assert_eq!(gen_store.get(beta), Some(String::from("Beta")));
        assert_eq!(gen_store.get(gamma), Some(String::from("Gamma")));
        assert_eq!(gen_store.get(delta), Some(String::from("Delta")));

        assert_eq!(gen_store.len(), 4_u32);
        assert_eq!(gen_store.get(Index { index: 1, generation: 2 } ), None);

        Ok(())
    }

    #[test]
    fn test_insert_get_remove() -> StdResult<()> {
        let mut storage = MockStorage::new();
        let mut gen_store = GenerationalStoreMut::attach_or_create(&mut storage)?;
        let alpha = gen_store.insert(String::from("Alpha"));
        let beta = gen_store.insert(String::from("Beta"));
        let gamma = gen_store.insert(String::from("Gamma"));

        assert_eq!(gen_store.len(), 3_u32);
        assert_eq!(gen_store.remove(beta.clone()), Ok(Some(String::from("Beta"))));
        assert_eq!(gen_store.len(), 2_u32);
        assert_eq!(gen_store.get(alpha), Some(String::from("Alpha")));
        assert_eq!(gen_store.get(beta), None);
        assert_eq!(gen_store.get(gamma), Some(String::from("Gamma")));

        let delta = gen_store.insert(String::from("Delta"));
        assert_eq!(gen_store.get(delta.clone()), Some(String::from("Delta")));
        assert_ne!(delta.clone(), Index{ index: 1, generation: 0 });
        assert_eq!(delta, Index{ index: 1, generation: 1 });

        Ok(())
    }

    #[test]
    fn test_iterator() -> StdResult<()> {
        let mut storage = MockStorage::new();
        let mut gen_store = GenerationalStoreMut::attach_or_create(&mut storage)?;
        gen_store.insert(1234);
        let second = gen_store.insert(2143);
        gen_store.insert(3412);
        gen_store.insert(4321);

        // iterate twice to make sure nothing changed
        assert_eq!(gen_store.iter().count(), 4);
        gen_store.remove(second)?;
        // count is still 4
        assert_eq!(gen_store.iter().count(), 4);
        let iter = gen_store.iter().filter(|item| matches!(item, (_, Entry::Occupied { .. })));
        assert_eq!(iter.count(), 3);

        // insert another in second's place
        gen_store.insert(5555);
        assert_eq!(gen_store.iter().count(), 4);

        // next one should increase the size
        gen_store.insert(6666);
        assert_eq!(gen_store.iter().count(), 5);

        Ok(())
    }
}