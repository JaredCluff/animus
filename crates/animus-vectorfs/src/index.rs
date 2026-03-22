use animus_core::error::{AnimusError, Result};
use animus_core::identity::SegmentId;
use hnsw_rs::prelude::*;
use parking_lot::RwLock;
use std::collections::HashMap;

/// Wrapper around hnsw_rs providing vector similarity search.
pub struct HnswIndex {
    /// The HNSW graph using cosine distance.
    hnsw: RwLock<Hnsw<f32, DistCosine>>,
    /// Map from internal HNSW data ID to SegmentId.
    id_map: RwLock<HashMap<usize, SegmentId>>,
    /// Reverse map from SegmentId to internal HNSW data ID.
    reverse_map: RwLock<HashMap<SegmentId, usize>>,
    /// Next internal ID to assign.
    next_id: RwLock<usize>,
    /// Vector dimensionality.
    dimensionality: usize,
}

impl HnswIndex {
    /// Create a new HNSW index for the given dimensionality.
    pub fn new(dimensionality: usize, max_elements: usize) -> Self {
        let max_nb_connection = 16;
        let ef_construction = 200;
        let nb_layer = 16;

        let hnsw = Hnsw::new(
            max_nb_connection,
            max_elements,
            nb_layer,
            ef_construction,
            DistCosine,
        );

        Self {
            hnsw: RwLock::new(hnsw),
            id_map: RwLock::new(HashMap::new()),
            reverse_map: RwLock::new(HashMap::new()),
            next_id: RwLock::new(0),
            dimensionality,
        }
    }

    /// Insert a vector for the given segment ID.
    pub fn insert(&self, segment_id: SegmentId, embedding: &[f32]) -> Result<()> {
        if embedding.len() != self.dimensionality {
            return Err(AnimusError::DimensionMismatch {
                expected: self.dimensionality,
                actual: embedding.len(),
            });
        }

        let internal_id = {
            let mut next = self.next_id.write();
            let id = *next;
            *next += 1;
            id
        };

        self.id_map.write().insert(internal_id, segment_id);
        self.reverse_map.write().insert(segment_id, internal_id);

        let data_vec = vec![(embedding, internal_id)];
        self.hnsw.write().parallel_insert(&data_vec);

        Ok(())
    }

    /// Search for the top-k nearest neighbors to the given query embedding.
    /// Returns (SegmentId, distance) pairs sorted by distance (ascending).
    pub fn search(&self, query: &[f32], top_k: usize) -> Result<Vec<(SegmentId, f32)>> {
        if query.len() != self.dimensionality {
            return Err(AnimusError::DimensionMismatch {
                expected: self.dimensionality,
                actual: query.len(),
            });
        }

        let ef_search = top_k.max(64);
        let hnsw = self.hnsw.read();
        let results = hnsw.search(query, top_k, ef_search);

        let id_map = self.id_map.read();
        let mapped: Vec<(SegmentId, f32)> = results
            .into_iter()
            .filter_map(|neighbour| {
                id_map
                    .get(&neighbour.d_id)
                    .map(|seg_id| (*seg_id, neighbour.distance))
            })
            .collect();

        Ok(mapped)
    }

    /// Remove a segment from the index.
    /// Note: hnsw_rs doesn't support true deletion — we track removed IDs
    /// and filter them from search results.
    pub fn remove(&self, segment_id: SegmentId) -> Result<()> {
        let internal_id = self
            .reverse_map
            .write()
            .remove(&segment_id)
            .ok_or(AnimusError::SegmentNotFound(segment_id.0))?;
        self.id_map.write().remove(&internal_id);
        Ok(())
    }

    pub fn len(&self) -> usize {
        self.id_map.read().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn dimensionality(&self) -> usize {
        self.dimensionality
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_insert_and_search() {
        let index = HnswIndex::new(4, 100);
        let id1 = SegmentId::new();
        let id2 = SegmentId::new();

        let v1 = vec![1.0, 0.0, 0.0, 0.0];
        let v2 = vec![0.0, 1.0, 0.0, 0.0];

        index.insert(id1, &v1).unwrap();
        index.insert(id2, &v2).unwrap();

        let results = index.search(&v1, 2).unwrap();
        assert_eq!(results.len(), 2);
        // First result should be v1 (closest to itself)
        assert_eq!(results[0].0, id1);
    }

    #[test]
    fn test_dimension_mismatch() {
        let index = HnswIndex::new(4, 100);
        let id = SegmentId::new();
        let wrong_dim = vec![1.0, 0.0];

        let result = index.insert(id, &wrong_dim);
        assert!(result.is_err());
    }

    #[test]
    fn test_remove() {
        let index = HnswIndex::new(4, 100);
        let id = SegmentId::new();
        index.insert(id, &[1.0, 0.0, 0.0, 0.0]).unwrap();
        assert_eq!(index.len(), 1);

        index.remove(id).unwrap();
        assert_eq!(index.len(), 0);

        // Search should return no results for removed segment
        let results = index.search(&[1.0, 0.0, 0.0, 0.0], 1).unwrap();
        assert!(results.is_empty());
    }
}
