use crate::unwrap_or_bug_message_hint;

#[cfg(not(feature = "more_numa_nodes"))]
pub(crate) const MAX_NUMA_NODES_SUPPORTED: usize = 64;
#[cfg(feature = "more_numa_nodes")]
pub(crate) const MAX_NUMA_NODES_SUPPORTED: usize = 1024;

const NUMA_NODE_TOO_LARGE: &'static str = "this hardware supports more NUMA-nodes than expected, use the `more_numa_nodes` feature to increase the limit";

struct DataPerNUMANodeManager<T>([T; MAX_NUMA_NODES_SUPPORTED]);

impl<T> DataPerNUMANodeManager<T> {
    pub(crate) const fn from_arr(inner: [T; MAX_NUMA_NODES_SUPPORTED]) -> Self {
        Self(inner)
    }

    pub(crate) fn get_ref_by_node(&self, numa_node: usize) -> &T {
        unwrap_or_bug_message_hint(
            self.0.get(numa_node),
            NUMA_NODE_TOO_LARGE
        )
    }

    pub(crate) fn get_mut_by_node(&mut self, numa_node: usize) -> &mut T {
        unwrap_or_bug_message_hint(
            self.0.get_mut(numa_node),
            NUMA_NODE_TOO_LARGE
        )
    }
}