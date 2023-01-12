# Deprecation of cookie option

## What?

As of 0.8.8 the `cookie` option is deprecated and it will be removed.
A new `auth` option was added.
If you don't use the `cookie` option, you're not affected and don't need to read this.
Note that this is different from `cookie_file`.

## Why?

The option was confusing:

* If you entered the path to cookie file (usually `~/.bitcoin/.cookie`), it wouldn't work.
* If you copied the contents of cookie file into it, `electrs` would break at the next restart of the system.
* If you used a script to fix the above run before `electrs` starts, it'd still break if `bitcoind` restarted for any reason.
* If you used `BindsTo` option of systemd, you'd solve the issue but introduce needless downtime and waste of performance.
* Entering `username:password` was the only valid use of `cookie` but it had nothing to do with cookie.

## What to do?

If you're installing `electrs` for the first time, just don't use `cookie`.
If you're updating, reconsider the motivation above.
If you used copying script, just use `cookie_file` to get the cookie directly.
If you also used `BindsTo`, we recommend removing it.
If you used fixed username and password because you didn't know about cookie or did it before `cookie_file` was implemented, reconsider using cookie authentication.
If you really have to use fixed username and password, specify them using `auth` option (`username:password` like before) and remove the `cookie` option.

## When the option will be removed?

Probably in a few months.
It'll still be detected and turned into explicit error for a while to make sure people really see the message and know what's going on.
You can see [the tracking issue #371](https://github.com/romanz/electrs/issues/371) to monitor the progress of the change.
