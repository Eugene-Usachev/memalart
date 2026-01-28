use std::ptr::NonNull;
use orengine_utils::hints::cold_path;

/// A minimalist intrusive singly linked list.
///
/// This structure is designed to manage objects that store the pointer
/// to the next element within themselves.
pub(crate) struct AllocatedInSelfList {
    head: Option<NonNull<AllocatedInSelfList>>,
}

impl AllocatedInSelfList {
    /// Creates a new `AllocatedInSelfList`.
    pub(crate) const fn new() -> Self {
        Self {
            head: None,
        }
    }

    /// Pushes a new element to the front of the list.
    ///
    /// # Safety
    ///
    /// The caller must ensure that `ptr` points to a valid and that the memory
    /// it points to remains valid while it is in the list.
    pub(crate) fn push(&mut self, ptr: NonNull<u8>) {
        let mut ptr: NonNull<AllocatedInSelfList> = ptr.cast();

        unsafe {
            ptr.as_mut().head = self.head;

            self.head = Some(ptr);
        }
    }

    /// Pops the front element from the list, if it exists.
    pub(crate) fn pop(&mut self) -> Option<NonNull<u8>> {
        match self.head {
            Some(mut ptr) => {
                cold_path();

                unsafe { self.head = ptr.as_mut().head; }

                Some(ptr.cast())
            }
            None => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ptr::NonNull;

    #[test]
    fn test_push_pop_lifo() {
        let mut list = AllocatedInSelfList::new();
        let mut region1 = [0; 40];
        let mut region2 = [0; 40];
        let ptr1 = NonNull::new(region1.as_mut_ptr()).unwrap();
        let ptr2 = NonNull::new(region2.as_mut_ptr()).unwrap();

        list.push(ptr1);
        list.push(ptr2);

        let popped1 = list.pop();
        assert_eq!(popped1, Some(ptr2));

        let popped2 = list.pop();
        assert_eq!(popped2, Some(ptr1));

        assert!(list.pop().is_none());
    }
}