# Contributing to electrs

:+1::tada: First off, thanks for taking the time to contribute! :tada::+1:

The following is a set of guidelines for contributing to this project! These are
mostly guidelines, not rules. Use your best judgment, and feel free to propose
changes to this document in a pull request.

## General

Electrs project operates an open contributor model where anyone is
welcome to contribute towards development in the form of peer review,
documentation, testing and patches.

Anyone is invited to contribute without regard to technical experience,
"expertise", OSS experience, age, or other concern. However, the development of
standards & reference implementations demands a high-level of rigor, adversarial
thinking, thorough testing and risk-minimization. Any bug may cost users real
money. That being said, we deeply welcome people contributing for the first time
to an open source project or pick up Rust while contributing. Don't be shy,
you'll learn.


## Contribution workflow

The codebase is maintained using the "contributor workflow" where everyone
without exception contributes patch proposals using "pull requests". This
facilitates social contribution, easy testing and peer review.

To contribute a patch, the workflow is a as follows:

1. Fork repository
2. Create topic branch
3. Commit patches

Please keep commits should atomic and diffs easy to read. For this reason
do not mix any formatting fixes or code moves with actual code changes.
Further, each commit, individually, should compile and pass tests, in order to
ensure git bisect and other automated tools function properly.

Please cover every new feature with unit tests.

When refactoring, structure your PR to make it easy to review and don't hesitate
to split it into multiple small, focused PRs.

To facilitate communication with other contributors, the project is making use
of GitHub's "assignee" field. First check that no one is assigned and then
comment suggesting that you're working on it. If someone is already assigned,
don't hesitate to ask if the assigned party or previous commenters are still
working on it if it has been awhile.


## Preparing PRs

The main library development happens in the `master` branch. This branch must
always compile without errors (using GitHub CI). All external contributions are
made within PRs into this branch.

Prerequisites that a PR must satisfy for merging into the `master` branch:
* final commit within the PR must compile and pass unit tests with no error
* final commit of the PR must be properly formatted and linted
* be based on the recent `master` tip from the original repository at
  <https://github.com/romanz/electrs>.

## Checking if the PR will pass the GitHub CI
PR authors may also find it useful to run the following script locally in order
to check that their code satisfies all the requirements of the GitHub workflows and doesn't fail
automated build.

You can run the following command from the root of the project:
```
./contrib/testChanges.sh
```


### Peer review

Anyone may participate in peer review which is expressed by comments in the pull
request. Typically, reviewers will review the code for obvious errors, as well as
test out the patch set and opine on the technical merits of the patch. Please,
first review PR on the conceptual level before focusing on code style or
grammar fixes.


### Formatting

The repository currently does use `rustfmt` for all the formatting needs. Running the automated
script mentioned above would format all your code for you :)

## Ending Notes
Get cracking, have fun, and do ask for help when in doubt!