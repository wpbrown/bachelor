# bachelor

**bachelor** provides high-performance, single-threaded memory management
utilities (specifically `Rc` and `Cell` alternatives) designed for
thread-per-core (TPC) architectures.

## Why bachelor?

In a TPC model, each thread is an isolated silo. Using standard `Arc` or `Mutex`
introduces unnecessary atomic overhead. `bachelor` focuses on "single life", the
fastest possible utilities for data that never needs to leave its home thread.

## Status

This crate is currently under active development.

