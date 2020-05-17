# Version 0.1.7

- Update `blocking` to `0.4.2`.
- Make `Task::blocking()` work without `run()`.

# Version 0.1.6

- Fix a deadlock by always re-registering `IoEvent`.

# Version 0.1.5

- Use `blocking` crate for blocking I/O.
- Fix a re-registration bug when in oneshot mode.
- Use eventfd on Linux.
- More tests.
- Fix timeout rounding error in epoll/wepoll.

# Version 0.1.4

- Fix a bug in UDS async connect

# Version 0.1.3

- Fix the writability check in async connect
- More comments and documentation
- Better security advice on certificates

# Version 0.1.2

- Improved internal docs, fixed typos, and more comments

# Version 0.1.1

- Upgrade dependencies

# Version 0.1.0

- Initial release
