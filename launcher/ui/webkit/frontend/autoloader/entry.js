/* SSPI entry routing and UI telemetry. Exploit selection remains in app.js. */
(function () {
  'use strict';
  var managerUrl = 'http://127.0.0.1:8084/';
  var firmware = /PlayStation 5\/(\d+\.\d+)/.exec(navigator.userAgent);
  var stopped = false;
  var pollTimer;
  var deadline = Date.now() + 120000;
  var sessionLabel = document.getElementById('sessionStatus');
  var managerLabel = document.getElementById('managerStatus');
  var managerLink = document.getElementById('openManager');
  var consoleTag = document.getElementById('consoleTag');
  if (consoleTag) consoleTag.textContent = firmware ? 'PS5 / ' + firmware[1] : 'Browser / firmware unavailable';

  function sessionIsReady(value) {
    return !!value && value.edition === 'sspi-payload-manager' && value.protocol === 1 && value.ready === true &&
      value.jailbreak === 'active' && value.evidence === 'privileged-manager-process' &&
      value.launcherTitleId === 'WKAL00001' && typeof value.sessionId === 'string' && /^\d+-\d+-\d+$/.test(value.sessionId);
  }

  function readJson(path, done) {
    var xhr = new XMLHttpRequest();
    var settled = false;
    var timeout;
    function finish(value, status, legacyUnsupported) {
      if (settled) return;
      settled = true;
      clearTimeout(timeout);
      if (!stopped) done(value, status || 0, legacyUnsupported === true);
    }
    timeout = setTimeout(function () { finish(null); try { xhr.abort(); } catch (e) { } }, 1600);
    try {
      xhr.open('GET', managerUrl + path + '?_=' + Date.now(), true);
      xhr.timeout = 1500;
      xhr.onload = function () {
        var value = null;
        try { if (xhr.status === 200) value = JSON.parse(xhr.responseText); } catch (e) { }
        finish(value, xhr.status, xhr.status === 200 && xhr.responseText.trim() === '404 Not Found');
      };
      xhr.onerror = xhr.ontimeout = function () { finish(null); };
      xhr.send();
    } catch (e) { finish(null); }
  }

  function readSession(done) {
    readJson('sspi/session', function (value, status, legacyUnsupported) {
      // A running pre-session SSPI Manager must not cause a second jailbreak.
      // 2.23.1 returned its exact not-found text with status 200. Other bad JSON,
      // new-session denial and timeouts never use this compatibility path.
      if (status === 404 || status === 501 || legacyUnsupported) {
        readJson('sspi/identity', function (identity) {
          var legacy = !!identity && identity.edition === 'sspi-payload-manager' && identity.protocol === 1 && identity.ready === true;
          done(legacy ? identity : null, legacy);
        });
      } else done(value, false);
    });
  }

  // Forward only console-hosted terminal messages. Cached/local sessions have no
  // Windows host; forwarding must never delay or fail the local launcher.
  var remoteHost = !!firmware && /^https?:$/.test(window.location.protocol) &&
    !/^(localhost|127(?:\.\d+){3}|\[?::1\]?)$/i.test(window.location.hostname) &&
    !/\.localhost$/i.test(window.location.hostname);
  var pendingLogs = [];
  var sending = false;
  function sendNextLog() {
    if (sending || stopped || !pendingLogs.length) return;
    sending = true;
    var xhr = new XMLHttpRequest();
    var settled = false;
    var timeout;
    function finish() {
      if (settled) return;
      settled = true;
      clearTimeout(timeout);
      sending = false;
      if (!stopped && pendingLogs.length) setTimeout(sendNextLog, 20);
    }
    var message = pendingLogs.shift();
    timeout = setTimeout(function () { finish(); try { xhr.abort(); } catch (e) { } }, 1600);
    try {
      xhr.open('POST', '/sspi/events', true);
      xhr.setRequestHeader('Content-Type', 'application/json');
      xhr.timeout = 1500;
      xhr.onload = xhr.onerror = xhr.ontimeout = finish;
      xhr.send(JSON.stringify({ message: message, stage: 'web-launcher' }));
    } catch (e) { finish(); }
  }
  window.sspiReport = function (message) {
    if (!remoteHost || stopped || pendingLogs.length >= 80) return;
    pendingLogs.push(String(message).slice(0, 1000));
    sendNextLog();
  };

  function showReady(legacy) {
    if (sessionLabel) {
      sessionLabel.textContent = legacy ? 'Not confirmed' : 'Active';
      sessionLabel.className = legacy ? '' : 'sspi-ready';
    }
    if (managerLabel) {
      managerLabel.textContent = legacy ? 'Ready · existing Manager' : 'Ready';
      managerLabel.className = 'sspi-ready';
    }
    if (managerLink) managerLink.hidden = false;
  }
  function monitorSession() {
    readSession(function (value, legacy) {
      if (sessionIsReady(value) || legacy) {
        showReady(legacy);
        window.location.replace(managerUrl);
        return;
      }
      if (Date.now() < deadline) pollTimer = setTimeout(monitorSession, 2500);
      else if (managerLabel) managerLabel.textContent = 'Not confirmed';
    });
  }
  function startLauncher() {
    if (sessionLabel) sessionLabel.textContent = 'Not confirmed';
    if (managerLabel) managerLabel.textContent = firmware ? 'Waiting for startup' : 'Not available';
    var script = document.createElement('script');
    script.src = 'app.js';
    script.onerror = function () {
      document.getElementById('progressLabel').textContent = 'Launcher files are unavailable. Reopen SSPI or run setup again.';
    };
    document.body.appendChild(script);
    if (firmware) pollTimer = setTimeout(monitorSession, 2500);
  }
  window.addEventListener('pagehide', function () { stopped = true; clearTimeout(pollTimer); pendingLogs.length = 0; });
  if (firmware) readSession(function (value, legacy) {
    if (sessionIsReady(value) || legacy) {
      showReady(legacy);
      document.getElementById('progressLabel').textContent = 'Opening Payload Manager…';
      window.location.replace(managerUrl);
    } else startLauncher();
  });
  else startLauncher();
})();
