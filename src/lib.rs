use std::{
    ops::{Deref, DerefMut, Range},
    pin::pin,
    task::{Context, Poll},
};

use futures::{task::noop_waker_ref, Future};

/// stream and index
pub struct Cursor<T> {
    /// stream of items. You must only grow this vector.
    pub stream: Vec<T>,
    pub index: usize,
}

impl<T> Cursor<T> {
    #[inline]
    pub fn remaining(&self) -> &[T] {
        &self.stream[self.index..]
    }
}

#[cfg(debug_assertions)]
pub struct Input<T>(std::cell::RefCell<Cursor<T>>);

#[cfg(not(debug_assertions))]
pub struct Input<T>(std::cell::UnsafeCell<Cursor<T>>);

impl<T> Input<T> {
    #[cfg(debug_assertions)]
    pub fn new(cursor: Cursor<T>) -> Self {
        Self(std::cell::RefCell::new(cursor))
    }
    #[cfg(not(debug_assertions))]
    pub fn new(cursor: Cursor<T>) -> Self {
        Self(std::cell::UnsafeCell::new(cursor))
    }

    #[cfg(debug_assertions)]
    /// Don't call .await while holding a borrow of the cursor.
    pub fn cursor(&self) -> impl Deref<Target = Cursor<T>> + '_ {
        self.0.borrow()
    }

    #[cfg(not(debug_assertions))]
    #[inline]
    /// Don't call .await while holding a borrow of the cursor.
    pub fn cursor(&self) -> impl Deref<Target = Cursor<T>> + '_ {
        unsafe { &*self.0.get() }
    }

    #[cfg(debug_assertions)]
    /// Don't call .await while holding a borrow of the cursor.
    pub fn cursor_mut(&self) -> impl DerefMut<Target = Cursor<T>> + '_ {
        self.0.borrow_mut()
    }

    #[cfg(not(debug_assertions))]
    #[inline]
    /// Don't call .await while holding a borrow of the cursor.
    pub fn cursor_mut(&self) -> impl DerefMut<Target = Cursor<T>> + '_ {
        unsafe { &mut *self.0.get() }
    }

    pub fn into_inner(self) -> Cursor<T> {
        self.0.into_inner()
    }

    pub fn read(&self) -> impl Future<Output = ()> + '_ {
        struct Read<'a, T> {
            input: &'a Input<T>,
            start_len: usize,
        }

        impl<T> Future for Read<'_, T> {
            type Output = ();

            fn poll(
                self: std::pin::Pin<&mut Self>,
                _cx: &mut std::task::Context<'_>,
            ) -> std::task::Poll<Self::Output> {
                let borrow = self.input.cursor();

                if borrow.stream.len() > self.start_len {
                    std::task::Poll::Ready(())
                } else {
                    std::task::Poll::Pending
                }
            }
        }

        Read {
            input: self,
            start_len: self.cursor().stream.len(),
        }
    }

    pub fn read_n(&self, at_least: usize) -> impl Future<Output = ()> + '_ {
        struct Read<'a, T> {
            input: &'a Input<T>,
            start_len: usize,
            at_least: usize,
        }

        impl<T> Future for Read<'_, T> {
            type Output = ();

            fn poll(
                self: std::pin::Pin<&mut Self>,
                _cx: &mut std::task::Context<'_>,
            ) -> std::task::Poll<Self::Output> {
                let borrow = self.input.cursor();

                if borrow.stream.len() >= self.start_len + self.at_least {
                    std::task::Poll::Ready(())
                } else {
                    std::task::Poll::Pending
                }
            }
        }

        Read {
            input: self,
            start_len: self.cursor().stream.len(),
            at_least,
        }
    }
}

pub async fn tag<T>(input: &Input<T>, tag: &[T]) -> Result<Range<usize>, ()>
where
    T: PartialEq,
{
    input.read_n(tag.len()).await;

    let mut cursor = input.cursor_mut();

    if cursor.remaining().starts_with(tag) {
        let start = cursor.index;
        cursor.index += tag.len();
        Ok(start..cursor.index)
    } else {
        Err(())
    }
}

pub async fn many0<T>(input: &Input<T>, mut cond: impl FnMut(&T) -> bool) -> Range<usize> {
    let start = input.cursor().index;

    loop {
        let mut cursor = input.cursor_mut();

        for (i, item) in cursor.stream[cursor.index..].iter().enumerate() {
            if !cond(item) {
                cursor.index += i;
                return start..cursor.index;
            }
        }

        cursor.index = cursor.stream.len();

        drop(cursor);

        input.read().await;
    }
}

pub trait PollNoop: Future + Unpin {
    fn poll_noop(&mut self) -> Poll<<Self as Future>::Output> {
        let mut cx = Context::from_waker(noop_waker_ref());

        pin!(self).poll(&mut cx)
    }
}

impl<T: Future + Unpin> PollNoop for T {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_read() {
        let input = Input::new(Cursor {
            stream: vec![1, 2, 3],
            index: 0,
        });

        let mut read = input.read();

        assert!(read.poll_noop().is_pending());

        input.cursor_mut().stream.push(4);

        assert!(read.poll_noop().is_ready());
    }

    #[test]
    fn test_get3() {
        async fn get3<T>(input: &Input<T>) {
            while input.cursor().remaining().len() < 3 {
                input.read().await;
            }
        }

        let input = Input::new(Cursor {
            stream: Vec::new(),
            index: 0,
        });
        let mut get3 = pin!(get3(&input));

        assert!(get3.poll_noop().is_pending());

        input.cursor_mut().stream.push(1);
        assert!(get3.poll_noop().is_pending());

        input.cursor_mut().stream.push(2);
        assert!(get3.poll_noop().is_pending());

        input.cursor_mut().stream.push(3);
        assert!(get3.poll_noop().is_ready());
    }

    #[test]
    fn test_many0() {
        let input = Input::new(Cursor {
            stream: Vec::new(),
            index: 0,
        });

        let cond = move |x: &i32| *x % 2 == 0;

        let mut p = pin!(many0(&input, cond));

        input.cursor_mut().stream.push(0);
        assert!(p.poll_noop().is_pending());

        input.cursor_mut().stream.push(2);
        assert!(p.poll_noop().is_pending());

        input.cursor_mut().stream.push(4);
        assert!(p.poll_noop().is_pending());

        input.cursor_mut().stream.push(1);
        assert_eq!(p.poll_noop(), Poll::Ready(0..3));
    }

    #[test]
    fn test_combined() {
        let input = Input::new(Cursor {
            stream: Vec::new(),
            index: 0,
        });

        let p = async {
            let alpha0 = many0(&input, |x: &u8| x.is_ascii_alphabetic()).await;
            dbg!(&alpha0);
            let digit = many0(&input, |x: &u8| x.is_ascii_digit()).await;
            dbg!(&digit);
            let alpha2 = many0(&input, |x: &u8| x.is_ascii_alphabetic()).await;

            (alpha0, digit, alpha2)
        };

        let mut p = pin!(p);

        input.cursor_mut().stream.push(b'a');
        input.cursor_mut().stream.push(b'b');
        input.cursor_mut().stream.push(b'c');

        assert!(p.poll_noop().is_pending());

        input.cursor_mut().stream.push(b'1');
        input.cursor_mut().stream.push(b'2');
        input.cursor_mut().stream.push(b'3');

        assert!(p.poll_noop().is_pending());
        input.cursor_mut().stream.push(b'a');
        input.cursor_mut().stream.push(b'b');
        input.cursor_mut().stream.push(b'c');

        assert!(p.poll_noop().is_pending());
        input.cursor_mut().stream.push(b';');

        assert_eq!(p.poll_noop(), Poll::Ready((0..3, 3..6, 6..9)));
    }

    #[test]
    #[should_panic]
    #[cfg(debug_assertions)]
    fn test_bad_borrow() {
        async fn bad(input: &Input<u8>) -> usize {
            let cursor = input.cursor();
            // You must not call read().await while borrowing cursor.
            // Can we ensure that it can't happen?
            input.read().await;
            cursor.remaining().len()
        }

        let input = Input::new(Cursor {
            stream: Vec::new(),
            index: 0,
        });

        let mut p = pin!(bad(&input));

        assert!(p.poll_noop().is_pending());

        input.cursor_mut().stream.push(1);
    }
}
