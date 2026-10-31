#!/usr/bin/env node
// `npx codexnotifier …` for people who type the name without the scope or the hyphen: hands over to
// @touchtap/codex-notifier, which runs the native binary. The hooks it installs call codex-notifier.
'use strict';
require('@touchtap/codex-notifier/bin/codex-notifier.js');
