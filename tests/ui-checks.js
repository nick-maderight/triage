(() => {
  const fail = (message) => {
      throw new Error(`Triage UI check failed: ${message}`);
    },
    assert = (condition, message) => {
      if (!condition) fail(message);
    },
    text = (selector, root = document) => root.querySelector(selector)?.textContent.trim() || '',
    visible = (element) =>
      Boolean(
        element &&
          element.getClientRects().length &&
          getComputedStyle(element).visibility !== 'hidden',
      ),
    waitFor = (predicate, message, timeout = 10000) =>
      new Promise((resolve, reject) => {
        let settled = false,
          observer,
          timer;
        const finish = (done) => {
            if (settled) return;
            settled = true;
            observer.disconnect();
            clearTimeout(timer);
            done();
          },
          check = () => {
            try {
              if (predicate()) finish(resolve);
            } catch (error) {
              finish(() => reject(error));
            }
          };
        observer = new MutationObserver(check);
        timer = setTimeout(
          () => finish(() => reject(new Error(`Timed out waiting for ${message}`))),
          timeout,
        );
        check();
        if (!settled)
          observer.observe(document.documentElement, {
            attributes: true,
            characterData: true,
            childList: true,
            subtree: true,
          });
      }),
    assertPeekLoaded = async () => {
      const dialog = document.querySelector('#peek');
      assert(dialog?.open, 'open the pane peek before running this scenario');
      await waitFor(
        () =>
          dialog.open &&
          text('#peek-state') !== 'connecting' &&
          document.querySelector('#peek-text')?.textContent !== 'Loading…',
        'loaded pane contents',
      );
      const pane = document.querySelector('#peek-text'),
        mode = document.querySelector('#peek-mode'),
        input = document.querySelector('#peek-input');
      assert(mode, 'peek mode button exists');
      assert(pane, 'pane contents exist');
      assert(input, 'peek input exists');
      return { dialog, pane, mode, input };
    },
    savePeekPreference = () => {
      try {
        return { readable: true, value: localStorage.getItem('triage-peek-mode') };
      } catch {
        return { readable: false };
      }
    },
    restorePeekPreference = (saved) => {
      if (!saved.readable) return;
      try {
        if (saved.value === null) localStorage.removeItem('triage-peek-mode');
        else localStorage.setItem('triage-peek-mode', saved.value);
      } catch {}
    },
    json = (body) =>
      Promise.resolve(
        new Response(JSON.stringify(body), { headers: { 'Content-Type': 'application/json' } }),
      ),
    keydown = (target, key) =>
      target.dispatchEvent(new KeyboardEvent('keydown', { key, bubbles: true, cancelable: true })),
    setQuery = (query) => {
      const search = document.querySelector('#picker-search');
      search.value = query;
      search.dispatchEvent(new Event('input', { bubbles: true }));
    },
    rows = () => document.querySelector('#picker-list').querySelectorAll('[role="option"]'),
    closed = (f, message) =>
      waitFor(
        () => !document.querySelector('#picker').open && f.getAttribute('aria-expanded') === 'false',
        message,
      ),
    checkPeek = async () => {
      const { dialog, pane, mode, input } = await assertPeekLoaded(),
        initialMode = mode.textContent.trim(),
        initialPreference = savePeekPreference(),
        initialInput = input.value,
        initialScrollTop = pane.scrollTop,
        initialScrollLeft = pane.scrollLeft;
      assert(
        initialMode === 'Fit' || initialMode === 'Readable',
        'peek mode button has an unexpected label',
      );
      try {
        if (mode.textContent.trim() === 'Readable') mode.click();
        assert(mode.textContent.trim() === 'Fit', 'peek starts in Fit mode');

        const rect = dialog.getBoundingClientRect(),
          paneStyle = getComputedStyle(pane),
          fontSize = Number.parseFloat(paneStyle.fontSize),
          lineHeight = Number.parseFloat(paneStyle.lineHeight),
          content = pane.firstElementChild;
        assert(
          Math.abs(rect.left) <= 1 &&
            Math.abs(rect.top) <= 1 &&
            Math.abs(rect.width - innerWidth) <= 1 &&
            Math.abs(rect.height - innerHeight) <= 1,
          'peek dialog fills the viewport within one pixel',
        );
        assert(paneStyle.whiteSpace === 'pre', 'pane preserves exact whitespace');
        assert(
          Math.abs(lineHeight / fontSize - 1.2) < 0.02,
          'Fit mode keeps a 1.2 line-height ratio',
        );
        assert(pane.scrollWidth <= pane.clientWidth, 'Fit mode has no horizontal overflow');
        const screenText = pane.textContent,
          rows = screenText.split('\n').length - (screenText.endsWith('\n') ? 1 : 0);
        assert(
          content && content.getBoundingClientRect().height + 1 >= rows * lineHeight,
          `pane reserves height for all ${rows} rows`,
        );

        mode.click();
        assert(mode.textContent.trim() === 'Readable', 'mode button switches to Readable');
        assert(
          Math.abs(Number.parseFloat(getComputedStyle(pane).fontSize) - 12) < 0.1,
          'Readable mode uses 12px text',
        );
        const readableStyle = getComputedStyle(pane),
          screenWidth = Math.max(...screenText.split('\n').map((line) => line.length)) * 12 * 0.6;
        assert(
          ['auto', 'scroll'].includes(readableStyle.overflowX) &&
            (screenWidth <= pane.clientWidth || pane.scrollWidth > pane.clientWidth),
          'Readable mode scrolls horizontally when the screen is wider than the pane',
        );

        document.body.dispatchEvent(
          new KeyboardEvent('keydown', { key: 'f', bubbles: true, cancelable: true }),
        );
        assert(mode.textContent.trim() === 'Fit', 'f outside a text field switches back to Fit');
        assert(
          pane.scrollWidth <= pane.clientWidth,
          'keyboard Fit mode removes horizontal overflow',
        );

        const fittedFontSize = getComputedStyle(pane).fontSize;
        input.value = 'ui-check f';
        input.dispatchEvent(
          new KeyboardEvent('keydown', { key: 'f', bubbles: true, cancelable: true }),
        );
        assert(mode.textContent.trim() === 'Fit', 'f in the text field does not toggle peek mode');
        assert(
          getComputedStyle(pane).fontSize === fittedFontSize,
          'f in the text field leaves pane font size unchanged',
        );
        return { scenario: 'peek', passed: true };
      } finally {
        input.value = initialInput;
        if (dialog.open && mode.textContent.trim() !== initialMode) mode.click();
        restorePeekPreference(initialPreference);
        pane.scrollTop = initialScrollTop;
        pane.scrollLeft = initialScrollLeft;
      }
    },
    checkDimensions = async () => {
      const { dialog, pane, mode } = await assertPeekLoaded(),
        initialMode = mode.textContent.trim(),
        initialPreference = savePeekPreference(),
        initialScrollTop = pane.scrollTop,
        initialScrollLeft = pane.scrollLeft,
        nativeFetch = window.fetch;
      assert(
        innerWidth >= 1200 && innerWidth > innerHeight,
        'dimensions scenario needs a wide desktop viewport',
      );
      assert(
        initialMode === 'Fit' || initialMode === 'Readable',
        'peek mode button has an unexpected label',
      );
      let fixture = { text: '─'.repeat(243), state: 'waiting', width: 243, height: 59 },
        peekRequests = 0;
      const fixtureFetch = function (input, init) {
        const request = input instanceof Request ? input : null,
          method = (init?.method || request?.method || 'GET').toUpperCase(),
          url = new URL(request?.url || String(input), location.href);
        if (
          url.origin === location.origin &&
          method === 'GET' &&
          /^\/v1\/(panes|dsh)\/[^/]+\/peek$/.test(url.pathname)
        ) {
          peekRequests++;
          return Promise.resolve(
            new Response(JSON.stringify(fixture), {
              headers: { 'Content-Type': 'application/json' },
            }),
          );
        }
        return nativeFetch.call(this, input, init);
      };
      const applyFixture = async (data, fits = true) => {
        const previousRequests = peekRequests;
        fixture = data;
        await waitFor(
          () => peekRequests > previousRequests && pane.textContent === data.text,
          'pane refresh with updated dimensions',
          5000,
        );
        const lineHeight = Number.parseFloat(getComputedStyle(pane).lineHeight),
          content = pane.firstElementChild;
        assert(content, 'pane content exists after fixture refresh');
        assert(
          content.getBoundingClientRect().height + 1 >= data.height * lineHeight,
          `pane reserves height for ${data.height} rows`,
        );
        if (fits)
          assert(
            pane.scrollWidth <= pane.clientWidth,
            'Fit mode has no horizontal overflow after refresh',
          );
        return Number.parseFloat(getComputedStyle(pane).fontSize);
      };
      try {
        if (mode.textContent.trim() === 'Readable') mode.click();
        assert(mode.textContent.trim() === 'Fit', 'dimensions scenario runs in Fit mode');
        window.fetch = fixtureFetch;
        const firstFontSize = await applyFixture(fixture);
        const secondFontSize = await applyFixture({
          text: '─'.repeat(289),
          state: 'waiting',
          width: 289,
          height: 63,
        });
        assert(secondFontSize < firstFontSize, 'Fit font shrinks when pane width grows');
        const wideFontSize = await applyFixture(
          { text: '─'.repeat(1000), state: 'waiting', width: 1000, height: 63 },
          false,
        );
        assert(
          mode.textContent.trim() === 'Readable',
          'Fit mode falls back to Readable below 4px',
        );
        assert(
          Math.abs(wideFontSize - 12) < 0.1,
          'Readable fallback uses 12px text',
        );
        assert(
          pane.scrollWidth > pane.clientWidth,
          'Readable fallback exposes horizontal scrolling',
        );
        return { scenario: 'dimensions', passed: true };
      } finally {
        if (window.fetch === fixtureFetch)
          await applyFixture(
            { text: '─'.repeat(289), state: 'waiting', width: 289, height: 63 },
            false,
          );
        window.fetch = nativeFetch;
        if (dialog.open && mode.textContent.trim() !== initialMode) mode.click();
        restorePeekPreference(initialPreference);
        pane.scrollTop = initialScrollTop;
        pane.scrollLeft = initialScrollLeft;
      }
    },
    checkOffline = async () => {
      const connection = document.querySelector('#connection'),
        main = document.querySelector('#main');
      await waitFor(
        () =>
          connection?.dataset.state === 'reconnecting' && text('h2', main) === 'Daemon unreachable',
        'offline empty state',
        15000,
      );
      assert(
        connection.dataset.state === 'reconnecting',
        'offline connection state is reconnecting',
      );
      assert(visible(main.querySelector('h2')), 'Daemon unreachable is visible');
      const retry = main.querySelector('p[role="status"]');
      assert(
        visible(retry) && /retry/i.test(retry.textContent),
        'offline state shows retry status',
      );
      assert(main.querySelector('article') === null, 'offline state contains no article cards');
      return { scenario: 'offline', passed: true };
    },
    checkRecovery = async () => {
      const connection = document.querySelector('#connection'),
        main = document.querySelector('#main');
      await waitFor(
        () => connection?.dataset.state === 'connected' && Boolean(main.querySelector('article')),
        'connected inbox with a card',
        45000,
      );
      assert(text('.connection-label') === 'Connected', 'recovered connection label is Connected');
      assert(
        [...main.querySelectorAll('article')].some(visible),
        'recovered inbox displays an article card',
      );
      return { scenario: 'recovery', passed: true };
    },
    checkFindingOther = async () => {
      const main = document.querySelector('#main'),
        card = main?.querySelector('article');
      assert(
        card?.dataset.kind === 'finding',
        'open a finding card before running this scenario',
      );
      const nativeFetch = window.fetch;
      let answerRequests = 0;
      window.fetch = function (input, init) {
        const request = input instanceof Request ? input : null,
          method = (init?.method || request?.method || 'GET').toUpperCase(),
          url = new URL(request?.url || String(input), location.href);
        if (
          url.origin === location.origin &&
          method === 'POST' &&
          /^\/v1\/cards\/[^/]+\/answer$/.test(url.pathname)
        ) {
          answerRequests++;
          return Promise.resolve(
            new Response(JSON.stringify({ error: 'ui check intercepted' }), {
              status: 409,
              headers: { 'Content-Type': 'application/json' },
            }),
          );
        }
        return nativeFetch.call(this, input, init);
      };
      const step = () => main.querySelector('.question-step')?.textContent || null,
        question = step(),
        hasOther = Boolean(card.querySelector('#other-btn'));
      try {
        let form = null;
        if (question) {
          assert(hasOther, 'finding questions offer Other');
          card.querySelector('#other-btn').click();
          await waitFor(() => main.querySelector('.other-form'), 'Other to open');
          form = main.querySelector('.other-form');
          form.querySelector('button[type="submit"]').click();
          await waitFor(
            () => [...document.querySelectorAll('#toasts .toast')].some((item) => item.textContent.includes('Type an answer first.')),
            'the empty Other message',
          );
          assert(step() === question, 'an empty Other submit stays on its question');
          const input = form.querySelector('#other-text');
          input.value = 'ui-check';
          input.dispatchEvent(new Event('input', { bubbles: true }));
          form.querySelector('button[type="submit"]').click();
          await waitFor(() => step() !== question, 'the custom answer to move past its question');
        } else {
          assert(!hasOther, 'plain finding cards do not offer Other');
          document.body.dispatchEvent(
            new KeyboardEvent('keydown', { key: 'o', bubbles: true, cancelable: true }),
          );
          assert(!main.querySelector('.other-form'), 'o does not open Other on plain finding cards');
        }
        assert(answerRequests === 0, 'Other on a finding card posts no answer');
        return { scenario: 'finding-other', passed: true, question };
      } finally {
        window.fetch = nativeFetch;
        const input = main.querySelector('#other-text');
        if (input) {
          input.value = '';
          input.dispatchEvent(new Event('input', { bubbles: true }));
          input.dispatchEvent(
            new KeyboardEvent('keydown', { key: 'Escape', bubbles: true, cancelable: true }),
          );
        }
      }
    },
    checkHistoryRetry = async () => {
      const main = document.querySelector('#main'),
        button = document.querySelector('#btn-history'),
        dialog = document.querySelector('#history'),
        list = document.querySelector('#history-list'),
        nativeFetch = window.fetch;
      assert(
        !main.querySelector('article') &&
          main.querySelector('p[role="status"]')?.textContent.startsWith('History unavailable:'),
        'history-retry needs an empty inbox with a previous history error',
      );
      let historyRequests = 0;
      const fixtureFetch = function (input, init) {
        const request = input instanceof Request ? input : null,
          method = (init?.method || request?.method || 'GET').toUpperCase(),
          url = new URL(request?.url || String(input), location.href);
        if (url.origin === location.origin && method === 'GET' && url.pathname === '/v1/history') {
          historyRequests++;
          return Promise.resolve(
            new Response(JSON.stringify({ cards: [], today_count: 0 }), {
              headers: { 'Content-Type': 'application/json' },
            }),
          );
        }
        return nativeFetch.call(this, input, init);
      };
      try {
        window.fetch = fixtureFetch;
        button.click();
        await waitFor(
          () => historyRequests > 0 && list.textContent.includes('No handled cards in recent history.'),
          'successful history retry',
        );
        assert(
          !main.textContent.includes('History unavailable:'),
          'successful history retry clears the empty-state error',
        );
        assert(
          main.textContent.includes('Handled today: 0'),
          'empty state displays history after a successful retry',
        );
        return { scenario: 'history-retry', passed: true };
      } finally {
        window.fetch = nativeFetch;
        if (dialog.open) dialog.close();
      }
    },
    checkComposeStaysOnQueue = async () => {
      const dialog = document.querySelector('#compose'),
        peek = document.querySelector('#peek'),
        nativeFetch = window.fetch;
      let started = 0;
      const fixtureFetch = function (input, init) {
        const request = input instanceof Request ? input : null,
          method = (init?.method || request?.method || 'GET').toUpperCase(),
          url = new URL(request?.url || String(input), location.href);
        if (url.origin === location.origin && method === 'POST' && url.pathname === '/v1/sessions') {
          started++;
          return Promise.resolve(
            new Response(JSON.stringify({ pane: '%fixture' }), {
              headers: { 'Content-Type': 'application/json' },
            }),
          );
        }
        return nativeFetch.call(this, input, init);
      };
      try {
        window.fetch = fixtureFetch;
        document.querySelector('#btn-compose').click();
        await waitFor(() => dialog.open, 'compose dialog open');
        document.querySelector('#compose-prompt').value = 'fixture prompt';
        document.querySelector('#compose-form').requestSubmit();
        await waitFor(
          () => started === 1 && !dialog.open && text('#announcer').includes('Started · %fixture'),
          'session started announcement',
        );
        assert(text('#toasts').includes('Started · %fixture'), 'a fast launch reports the pane it started');
        await new Promise((resolve) => setTimeout(resolve, 500));
        assert(!peek.open, 'starting a session leaves the peek panel closed');
        return { scenario: 'compose-stays', passed: true };
      } finally {
        window.fetch = nativeFetch;
        if (dialog.open) dialog.close();
        if (peek.open) peek.close();
      }
    },
    checkPicker = async () => {
      const dialog = document.querySelector('#compose'),
        picker = document.querySelector('#picker'),
        search = document.querySelector('#picker-search'),
        list = document.querySelector('#picker-list'),
        nativeFetch = window.fetch;
      let posted = null;
      const fixtureFetch = function (input, init) {
          const request = input instanceof Request ? input : null,
            method = (init?.method || request?.method || 'GET').toUpperCase(),
            url = new URL(request?.url || String(input), location.href);
          if (url.origin !== location.origin) return nativeFetch.call(this, input, init);
          if (method === 'GET' && url.pathname === '/v1/models')
            return json({
              models: [
                { selector: 'fixture/alpha-1', name: 'Alpha One' },
                { selector: 'fixture/beta-2', name: 'Beta Two' },
                ...Array.from({ length: 20 }, (_, i) => ({
                  selector: `fixture/filler-${i}`,
                  name: `Filler ${i}`,
                })),
              ],
              default: 'fixture/alpha-1',
              thinking: ['low', 'high', 'max'],
              default_thinking: 'max',
            });
          if (method === 'GET' && url.pathname === '/v1/tmux-sessions')
            return json({
              sessions: ['main', 'work'],
              default: 'main',
              directories: ['/srv/fixture/a', '/srv/fixture/b'],
            });
          if (method === 'POST' && url.pathname === '/v1/sessions') {
            posted = JSON.parse(init?.body ?? 'null');
            return json({ pane: '%fixture' });
          }
          return nativeFetch.call(this, input, init);
        };
      try {
        window.fetch = fixtureFetch;
        document.querySelector('#btn-compose').click();
        await waitFor(
          () => dialog.open && text('#compose-model').includes('Alpha One'),
          'compose open with models loaded',
        );
        const model = document.querySelector('#compose-model');
        model.click();
        await waitFor(() => picker.open && rows().length === 23, 'model picker open with 23 rows');
        assert(text('#picker-title') === 'Model', 'picker title reads Model');
        assert(rows()[0].hasAttribute('data-current'), 'first row is the current value');
        assert(rows()[0].getAttribute('aria-selected') === 'true', 'first row starts active');
        await new Promise((resolve) => setTimeout(resolve, 250));
        const pickerRect = picker.getBoundingClientRect(),
          listRect = list.getBoundingClientRect(),
          modelRect = model.getBoundingClientRect();
        assert(list.scrollHeight > list.clientHeight, 'a long option list scrolls');
        assert(
          listRect.bottom <= pickerRect.bottom + 1,
          'the list stays inside the picker bottom',
        );
        if (innerWidth <= 639) {
          assert(pickerRect.left === 0, 'phone picker starts at the left edge');
          assert(Math.round(pickerRect.width) === innerWidth, 'phone picker spans the viewport width');
          assert(
            Math.abs(pickerRect.top - (window.visualViewport?.offsetTop ?? 0)) <= 1,
            'phone picker top matches the visual viewport',
          );
          assert(visible(document.querySelector('#picker .sheet-head')), 'phone picker shows its header');
          assert(rows()[1].getBoundingClientRect().height >= 44, 'phone rows are at least 44px tall');
          assert(modelRect.height >= 44, 'the field is at least 44px tall');
        } else {
          assert(!visible(document.querySelector('#picker .sheet-head')), 'desktop picker hides its header');
          assert(
            pickerRect.top >= modelRect.bottom || pickerRect.bottom <= modelRect.top,
            'desktop picker hangs off its field',
          );
        }
        setQuery('beta');
        await waitFor(() => rows().length === 2, 'beta query narrows to two rows');
        assert(text('#picker-list [role="option"]').startsWith('Use'), 'a free field offers the typed text');
        assert(rows()[1].getAttribute('aria-selected') === 'true', 'the match starts active ahead of the Use row');
        keydown(search, 'Enter');
        await closed(model, 'model picker close');
        assert(model.value === 'fixture/beta-2', 'model field holds the chosen selector');
        assert(text('#compose-model').includes('Beta Two'), 'model field shows the display name');
        assert(document.activeElement === model, 'focus returns to the field');
        const thinking = document.querySelector('#compose-thinking');
        thinking.click();
        await waitFor(() => picker.open, 'thinking picker open');
        setQuery('turbo');
        await waitFor(() => rows().length === 0, 'turbo matches nothing');
        assert(
          !document.querySelector('#picker-empty').hidden && text('#picker-empty') === 'No match',
          'a strict field shows No match',
        );
        keydown(search, 'Enter');
        await new Promise((resolve) => setTimeout(resolve, 200));
        assert(picker.open, 'Enter with no match keeps the picker open');
        keydown(search, 'Escape');
        await closed(thinking, 'thinking picker close');
        assert(dialog.open, 'compose stays open after picker Escape');
        assert(thinking.value === '', 'thinking stays blank');
        const cwd = document.querySelector('#compose-cwd');
        cwd.click();
        await waitFor(() => picker.open && rows().length === 3, 'cwd picker open with three rows');
        setQuery('/srv/custom');
        await waitFor(() => rows().length === 1, 'typed path leaves one Use row');
        assert(rows()[0].getAttribute('aria-selected') === 'true', 'the Use row starts active');
        keydown(search, 'Enter');
        await closed(cwd, 'cwd picker close');
        cwd.click();
        await waitFor(() => picker.open, 'cwd picker reopens');
        assert(search.value === '/srv/custom', 'a typed value prefills the query');
        assert(rows()[0].hasAttribute('data-current'), 'the typed value row is current');
        keydown(search, 'Escape');
        await closed(cwd, 'cwd picker close');
        document.querySelector('#compose-prompt').value = 'fixture prompt';
        document.querySelector('#compose-form').requestSubmit();
        await waitFor(() => posted, 'session body posted');
        assert(posted.cwd === '/srv/custom', 'posted cwd is the typed path');
        assert(posted.model === 'fixture/beta-2', 'posted model is the chosen selector');
        assert(posted.thinking === null && posted.session === null, 'blank fields post null');
        document.querySelector('#btn-compose').click();
        await waitFor(() => dialog.open, 'compose reopens');
        model.click();
        await waitFor(() => picker.open, 'model picker reopens');
        rows()[0].click();
        await closed(model, 'model reset close');
        cwd.click();
        await waitFor(() => picker.open, 'cwd picker reopens for reset');
        setQuery('');
        await waitFor(() => rows().length === 3, 'cwd rows restored');
        rows()[0].click();
        await closed(cwd, 'cwd reset close');
        assert(model.value === '' && cwd.value === '', 'Default leaves both fields blank');
        assert(text('#compose-model').startsWith('Default'), 'model field shows Default');
        return { scenario: 'picker', passed: true };
      } finally {
        window.fetch = nativeFetch;
        if (picker.open) picker.close();
        if (dialog.open) dialog.close();
      }
    },
    // A catalog read can outlive the engine choice that asked for it. A dsh bridge that is timing
    // out holds its answer for seconds, so an answer for one engine must never fill the model field
    // that now names another, and the field must still take it when its engine is the chosen one.
    checkModelEngine = async () => {
      const dialog = document.querySelector('#compose'),
        picker = document.querySelector('#picker'),
        engine = document.querySelector('#compose-engine'),
        model = document.querySelector('#compose-model'),
        nativeFetch = window.fetch,
        omp = {
          engine: 'omp',
          models: [
            { selector: 'fixture/omp-alpha', name: 'OMP Alpha' },
            { selector: 'fixture/omp-beta', name: 'OMP Beta' },
          ],
          default: 'fixture/omp-alpha',
          thinking: ['high'],
          default_thinking: 'high',
          execution_models: [],
        },
        dsh = {
          engine: 'dsh',
          models: [{ selector: 'dsh/only-model', name: 'DSH Only' }],
          default: 'dsh/only-model',
          thinking: ['low'],
          default_thinking: 'low',
          execution_models: [],
        },
        // The dsh catalog answers only when the check releases it, the way a timing-out bridge
        // holds its answer while the sheet moves on.
        held = { answer: null, released: false, requested: false },
        holdDsh = () =>
          new Promise((resolve) => {
            held.requested = true;
            held.answer = () => {
              held.released = true;
              resolve(json(dsh));
            };
            if (held.release) held.answer();
          }),
        fixtureFetch = function (input, init) {
          const request = input instanceof Request ? input : null,
            method = (init?.method || request?.method || 'GET').toUpperCase(),
            url = new URL(request?.url || String(input), location.href);
          if (url.origin !== location.origin) return nativeFetch.call(this, input, init);
          if (method === 'GET' && url.pathname === '/v1/models' && url.searchParams.get('engine') === 'dsh')
            return holdDsh();
          if (method === 'GET' && url.pathname === '/v1/models') return json(omp);
          if (method === 'GET' && url.pathname === '/v1/tmux-sessions')
            return json({ sessions: ['main'], default: 'main', directories: ['/srv/fixture/a'] });
          return nativeFetch.call(this, input, init);
        },
        // The picker's close event waits on its exit transition, which a page that never paints
        // never runs, so these waits read the dialog's state rather than that event.
        closePicker = () => waitFor(() => !picker.open, 'model picker close'),
        // A held read changes no DOM, so the mutation-driven waitFor would never notice it.
        until = async (predicate, message) => {
          for (let i = 0; i < 200; i += 1) {
            if (predicate()) return;
            await new Promise((resolve) => setTimeout(resolve, 50));
          }
          fail(message);
        },
        chooseEngine = async (value) => {
          engine.click();
          await waitFor(() => picker.open, `engine picker open for ${value}`);
          const row = [...rows()].find((option) => option.textContent.trim() === value);
          assert(row, `engine row ${value} is offered`);
          row.click();
          await waitFor(() => !picker.open && engine.value === value, `engine picker closed on ${value}`);
        },
        listed = async () => {
          model.click();
          await waitFor(() => picker.open, 'model picker open');
          return [...rows()].map((row) => row.textContent.trim());
        };
      try {
        window.fetch = fixtureFetch;
        document.querySelector('#btn-compose').click();
        await waitFor(
          () => dialog.open && text('#compose-model').includes('OMP Alpha'),
          'compose open with the omp catalog',
        );
        await chooseEngine('dsh');
        await until(() => held.requested, 'the dsh catalog read starts');
        await chooseEngine('omp');
        await waitFor(
          () => text('#compose-model').includes('OMP Alpha'),
          'the omp catalog lands while the dsh read is still held',
        );
        // The dsh read answers now, long after the engine went back to omp.
        held.answer();
        await new Promise((resolve) => setTimeout(resolve, 300));
        assert(held.released, 'the held dsh catalog answered');
        const settled = await listed();
        assert(
          settled.some((row) => row.includes('OMP Beta')),
          'the omp field keeps the omp catalog after a late dsh answer',
        );
        assert(
          !settled.some((row) => row.includes('DSH Only')),
          'a late dsh catalog never fills the omp model list',
        );
        keydown(document.querySelector('#picker-search'), 'Escape');
        await closePicker();
        assert(text('#compose-model').includes('OMP Alpha'), 'the omp field names the omp default');
        // The same held read must land when dsh is the engine the field names.
        held.release = true;
        await chooseEngine('dsh');
        await waitFor(() => text('#compose-model').includes('DSH Only'), 'the dsh catalog fills the dsh field');
        const dshRows = await listed();
        assert(
          dshRows.some((row) => row.includes('DSH Only')),
          'the dsh field offers the dsh catalog it asked for',
        );
        assert(
          !dshRows.some((row) => row.includes('OMP Beta')),
          'the dsh field offers no omp models',
        );
        keydown(document.querySelector('#picker-search'), 'Escape');
        await closePicker();
        assert(engine.value === 'dsh', 'the engine field keeps the chosen engine');
        return { scenario: 'models', passed: true };
      } finally {
        window.fetch = nativeFetch;
        if (picker.open) picker.close();
        if (dialog.open) dialog.close();
      }
    },
    checkPlanApproval = async () => {
      await waitFor(() => document.querySelector('.card[data-kind="plan"]'), 'a plan card in the queue');
      const picker = document.querySelector('#picker'),
        field = document.querySelector('#plan-executor'),
        nativeFetch = window.fetch,
        models = [
          { selector: '@default', name: 'default role (Fixture Model)' },
          { selector: '@smol', name: 'smol role (Fixture Model)' },
        ];
      let posted = null;
      const fixtureFetch = function (input, init) {
        const request = input instanceof Request ? input : null,
          method = (init?.method || request?.method || 'GET').toUpperCase(),
          url = new URL(request?.url || String(input), location.href);
        if (url.origin !== location.origin) return nativeFetch.call(this, input, init);
        if (method === 'GET' && url.pathname === '/v1/models')
          return json({ models, execution_models: models, default: '@default', thinking: ['low'], default_thinking: 'low' });
        if (method === 'POST' && /^\/v1\/cards\/[^/]+\/answer$/.test(url.pathname)) {
          posted = JSON.parse(init?.body ?? '{}');
          return json({ ok: true });
        }
        return nativeFetch.call(this, input, init);
      };
      try {
        window.fetch = fixtureFetch;
        assert(field, 'an answerable plan card shows its Execution model field');
        field.click();
        await waitFor(() => picker.open && rows().length === 3, 'execution roles loaded in the picker');
        assert(text('#picker-title') === 'Execution model', 'picker title identifies the plan execution model');
        assert(rows()[1].textContent.includes('@default') || rows()[1].textContent.includes('default role'), 'cycle role appears after Default');
        setQuery('smol');
        await waitFor(() => rows().length === 1, 'smol role search');
        keydown(document.querySelector('#picker-search'), 'Enter');
        await closed(field, 'execution model picker closes');
        assert(field.value === '@smol', 'picker stores selected execution role');
        document.querySelector('#opt-0').click();
        await waitFor(() => posted !== null, 'approval request is posted');
        assert(posted.option === 0 && posted.executor === '@smol', 'Approve posts selected role');
        await waitFor(() => document.querySelector('#plan-executor'), 'mocked plan card returns after refresh');
        return { scenario: 'plan-approval', passed: true };
      } finally {
        window.fetch = nativeFetch;
        if (picker.open) picker.close();
      }
    },
    checkPlanInto = async () => {
      await waitFor(() => document.querySelector('.card'), 'a card in the queue');
      const compose = document.querySelector('#compose'),
        picker = document.querySelector('#picker'),
        dispatch = document.querySelector('#dispatch'),
        nativeFetch = window.fetch;
      let posted = null;
      const fixtureFetch = function (input, init) {
          const request = input instanceof Request ? input : null,
            method = (init?.method || request?.method || 'GET').toUpperCase(),
            url = new URL(request?.url || String(input), location.href);
          if (url.origin !== location.origin) return nativeFetch.call(this, input, init);
          if (method === 'GET' && url.pathname === '/v1/models') {
            if (url.searchParams.get('engine') === 'dsh')
              return json({
                engine: 'dsh',
                models: [{ selector: 'fixture/fast', name: 'Dsh Fast', efforts: ['low', 'high'] }],
                execution_models: [
                  { selector: 'fixture/fast', name: 'Dsh Fast', efforts: ['low', 'high'] },
                ],
                default: 'fixture/fast',
                thinking: ['low', 'high'],
              });
            return json({
              engine: 'omp',
              models: [
                { selector: 'fixture/alpha-1', name: 'Alpha One' },
                { selector: 'fixture/beta-2', name: 'Beta Two' },
                ...Array.from({ length: 20 }, (_, i) => ({
                  selector: `fixture/filler-${i}`,
                  name: `Filler ${i}`,
                })),
              ],
              default: 'fixture/alpha-1',
              thinking: ['low', 'high', 'max'],
              default_thinking: 'max',
            });
          }
          if (method === 'GET' && url.pathname === '/v1/tmux-sessions')
            return json({
              sessions: ['main', 'work'],
              default: 'main',
              directories: ['/srv/fixture/a', '/srv/fixture/b'],
            });
          if (method === 'POST' && url.pathname === '/v1/sessions') {
            posted = JSON.parse(init?.body ?? 'null');
            return json({ pane: '%fixture' });
          }
          return nativeFetch.call(this, input, init);
        };
      const segmentRow = (group) =>
        group.scrollWidth <= group.clientWidth &&
        [...group.children].every((b) => b.getClientRects().length === 1);
      try {
        window.fetch = fixtureFetch;
        document.querySelector('#compose-mode-goal').click();
        document.querySelector('#btn-compose').click();
        await waitFor(
          () => compose.open && text('#compose-model').includes('Alpha One'),
          'compose open with models loaded',
        );
        assert(compose.querySelector('#compose-models').hidden, 'Goal hides the execution model field');
        document.querySelector('#compose-mode-plan-into').click();
        assert(!compose.querySelector('#compose-models').hidden, 'Plan into shows the execution model field');
        assert(/approv/.test(text('#compose-mode-help')), 'the compose help mentions approval');
        assert(segmentRow(compose.querySelector('#compose-mode')), 'the three compose segments share one row');
        const executor = document.querySelector('#compose-executor');
        executor.click();
        await waitFor(() => picker.open, 'execution picker open');
        assert(text('#picker-title') === 'Execution model', 'picker title reads Execution model');
        assert(
          text('#picker-list [role="option"]').includes('omp smol role'),
          'the Default row names the omp smol role',
        );
        setQuery('beta');
        await waitFor(() => rows().length === 2, 'beta query narrows to two rows');
        keydown(document.querySelector('#picker-search'), 'Enter');
        await closed(executor, 'execution picker close');
        assert(executor.value === 'fixture/beta-2', 'executor holds the chosen selector');
        const effort = document.querySelector('#compose-executor-effort');
        assert(visible(effort), 'Plan into shows the execution effort field');
        effort.click();
        await waitFor(() => picker.open, 'execution effort picker open');
        assert(text('#picker-title') === 'Execution effort', 'picker title reads Execution effort');
        setQuery('high');
        await waitFor(() => rows().length === 1, 'high narrows to one effort row');
        keydown(document.querySelector('#picker-search'), 'Enter');
        await closed(effort, 'execution effort picker close');
        assert(effort.value === 'high', 'execution effort holds high');
        document.querySelector('#compose-prompt').value = 'fixture prompt';
        compose.querySelector('#compose-form').requestSubmit();
        await waitFor(() => posted, 'plan-into session body posted');
        assert(posted.mode === 'plan-into', 'posted mode is plan-into');
        assert(posted.executor === 'fixture/beta-2', 'posted executor is the chosen selector');
        assert(posted.executor_thinking === 'high', 'posted execution effort is the chosen level');

        posted = null;
        document.querySelector('#btn-compose').click();
        await waitFor(() => compose.open, 'compose reopens');
        executor.click();
        await waitFor(() => picker.open, 'execution picker reopens');
        rows()[0].click();
        await closed(executor, 'execution reset close');
        assert(executor.value === '', 'Default resets the executor');
        // A dsh sheet offers no execution effort until it names an execution model.
        const engineField = document.querySelector('#compose-engine');
        engineField.click();
        await waitFor(() => picker.open, 'engine picker open on dsh');
        setQuery('dsh');
        await waitFor(() => rows().length === 1, 'dsh narrows to one engine row');
        keydown(document.querySelector('#picker-search'), 'Enter');
        await closed(engineField, 'engine picker close');
        assert(engineField.value === 'dsh', 'the compose engine holds dsh');
        await waitFor(() => text('#compose-model').includes('Dsh Fast'), 'dsh models load');
        assert(
          text('#compose-executor-effort').includes('needs an execution model'),
          'a dsh effort names the execution model it waits for',
        );
        effort.click();
        await waitFor(() => picker.open, 'dsh execution effort picker open');
        assert(rows().length === 1, 'the dsh execution effort lists only Default');
        assert(
          text('#picker-empty').includes('Nothing to choose'),
          'the dsh effort picker says there is nothing to choose yet',
        );
        picker.close();
        executor.click();
        await waitFor(() => picker.open, 'dsh execution picker open');
        setQuery('fast');
        await waitFor(
          () => rows().length === 2,
          'dsh model query narrows to the model and its free-text row',
        );
        keydown(document.querySelector('#picker-search'), 'Enter');
        await closed(executor, 'dsh execution picker close');
        assert(executor.value === 'fixture/fast', 'the dsh executor holds the chosen model');
        effort.click();
        await waitFor(() => picker.open, 'dsh execution effort picker reopens');
        setQuery('high');
        await waitFor(() => rows().length === 1, 'the dsh model effort narrows to one row');
        keydown(document.querySelector('#picker-search'), 'Enter');
        await closed(effort, 'dsh execution effort picker close');
        assert(effort.value === 'high', 'the dsh execution effort holds high');
        engineField.click();
        await waitFor(() => picker.open, 'engine picker reopens');
        rows()[0].click();
        await closed(engineField, 'engine picker reset close');
        assert(engineField.value === '', 'Default returns the compose engine');
        await waitFor(() => text('#compose-model').includes('Alpha One'), 'omp models reload');
        document.querySelector('#compose-mode-goal').click();
        assert(compose.querySelector('#compose-models').hidden, 'Goal hides the execution model again');
        document.querySelector('#compose-prompt').value = 'fixture prompt goal';
        compose.querySelector('#compose-form').requestSubmit();
        await waitFor(() => posted, 'goal session body posted');
        assert(posted.mode === 'goal', 'posted mode is goal');
        assert(!('executor' in posted), 'goal posts no executor key');

        document.dispatchEvent(new KeyboardEvent('keydown', { key: 'P', bubbles: true }));
        await waitFor(() => dispatch.open, 'dispatch opens on P');
        const mode = (name) => document.querySelector(`#dispatch-mode-${name}`);
        assert(mode('plan-into').getAttribute('aria-checked') === 'true', 'P preselects Plan into');
        assert(!dispatch.querySelector('#dispatch-models').hidden, 'Plan into shows the execution model field');
        assert(visible(document.querySelector('#dispatch-executor-effort')), 'dispatch shows the execution effort field');
        assert(segmentRow(dispatch.querySelector('#dispatch-mode')), 'the four dispatch segments share one row');
        assert(text('#dispatch-model').includes('Alpha One'), 'the Model Default names the daemon model');
        assert(text('#dispatch-thinking').includes('max'), 'the Thinking Default names the daemon level');
        const dshEngine = document.querySelector('#dispatch-engine').value === 'dsh';
        assert(
          text('#dispatch-executor').includes(dshEngine ? 'planner model' : 'omp smol role'),
          'the executor Default names the default of the chosen engine',
        );
        dispatch.querySelector('#dispatch-start').scrollIntoView({ block: 'center' });
        const dialogRect = dispatch.getBoundingClientRect(),
          startRect = dispatch.querySelector('#dispatch-start').getBoundingClientRect();
        assert(
          startRect.bottom <= dialogRect.bottom + 1 && startRect.top >= dialogRect.top - 1,
          'the Launch button stays inside the dialog',
        );
        mode('act').click();
        assert(dispatch.querySelector('#dispatch-models').hidden, 'Act hides the execution model field');
        assert(text('#dispatch-model').includes('Alpha One'), 'Act keeps the Model field visible');
        assert(text('#dispatch-thinking').includes('max'), 'Act keeps the Thinking field visible');
        return { scenario: 'plan-into', passed: true };
      } finally {
        window.fetch = nativeFetch;
        if (picker.open) picker.close();
        if (dispatch.open) dispatch.close();
        if (compose.open) compose.close();
      }
    },
    checkDispatchDraft = async () => {
      await waitFor(() => document.querySelector('.card'), 'a card to dispatch');
      const dialog = document.querySelector('#dispatch'),
        note = document.querySelector('#dispatch-note'),
        mode = (name) => document.querySelector(`#dispatch-mode-${name}`),
        press = (key) =>
          document.dispatchEvent(new KeyboardEvent('keydown', { key, bubbles: true }));
      try {
        press('i');
        await waitFor(() => dialog.open, 'dispatch dialog open');
        assert(mode('investigate').getAttribute('aria-checked') === 'true', 'Investigate is preselected');
        assert(mode('act').getAttribute('aria-checked') === 'false', 'Act starts unchecked');
        mode('act').click();
        assert(mode('act').getAttribute('aria-checked') === 'true', 'Act is selected after click');
        assert(mode('investigate').getAttribute('aria-checked') === 'false', 'Investigate is cleared');
        note.value = 'fixture instruction\nsecond line';
        note.dispatchEvent(new Event('input', { bubbles: true }));
        dialog.close();
        assert(!dialog.open, 'the dispatch dialog closes');
        document.querySelector('.dock .btn[data-od-id^="button-dispatch"]').click();
        await waitFor(() => dialog.open, 'dispatch dialog reopened from the dock');
        assert(note.value === 'fixture instruction\nsecond line', 'the instruction draft persists');
        assert(mode('act').getAttribute('aria-checked') === 'true', 'Act mode persists');
        dialog.close();
        press('A');
        await waitFor(() => dialog.open, 'dispatch dialog open from A');
        assert(mode('act').getAttribute('aria-checked') === 'true', 'A preselects Act');
        return { scenario: 'dispatch', passed: true };
      } finally {
        if (dialog.open) dialog.close();
      }
    },
    checkPortrait = async () => {
      assert(
        innerWidth <= 639 && innerHeight > innerWidth,
        'portrait scenario needs a narrow portrait viewport',
      );
      const { pane, mode } = await assertPeekLoaded(),
        hint = document.querySelector('#peek-fit-hint');
      assert(mode.textContent.trim() === 'Readable', 'portrait peek opens in Readable mode');
      assert(
        Math.abs(Number.parseFloat(getComputedStyle(pane).fontSize) - 12) < 0.1,
        'portrait Readable mode uses 12px text',
      );
      assert(pane.scrollWidth > pane.clientWidth, 'portrait peek exposes horizontal scrolling');
      const landscapeFits = Math.max(innerWidth, innerHeight) / (pane.scrollWidth / 12) >= 8;
      assert(
        landscapeFits ? !hint.hidden && visible(hint) : hint.hidden,
        landscapeFits
          ? 'portrait peek shows its landscape hint'
          : 'portrait peek hides the landscape hint when landscape would not fit either',
      );
      return { scenario: 'portrait', passed: true };
    },
    checkLandscape = async () => {
      assert(
        innerWidth > 639 && innerWidth > innerHeight,
        'landscape scenario needs a wide landscape viewport',
      );
      const { dialog, pane, mode } = await assertPeekLoaded(),
        initialMode = mode.textContent.trim(),
        initialPreference = savePeekPreference(),
        initialScrollTop = pane.scrollTop,
        initialScrollLeft = pane.scrollLeft;
      assert(
        initialMode === 'Fit' || initialMode === 'Readable',
        'peek mode button has an unexpected label',
      );
      try {
        if (mode.textContent.trim() === 'Fit') mode.click();
        mode.click();
        assert(mode.textContent.trim() === 'Fit', 'landscape mode button switches to Fit');
        assert(
          pane.scrollWidth <= pane.clientWidth,
          'landscape Fit mode has no horizontal overflow',
        );
        return { scenario: 'landscape', passed: true };
      } finally {
        if (dialog.open && mode.textContent.trim() !== initialMode) mode.click();
        restorePeekPreference(initialPreference);
        pane.scrollTop = initialScrollTop;
        pane.scrollLeft = initialScrollLeft;
      }
    },
    checkMobileCard = async () => {
      assert(innerWidth <= 639, 'mobile-card scenario needs a viewport at most 639px wide');
      await waitFor(() => document.querySelector('.card .option'), 'a card with options');
      const card = document.querySelector('.card'),
        scroll = card.querySelector('.card-scroll'),
        top = (e) => e.getBoundingClientRect().top + scrollY;
      assert(
        document.documentElement.scrollWidth <= innerWidth,
        'the page has no horizontal overflow',
      );
      assert(
        getComputedStyle(scroll).overflowY === 'visible' ||
          scroll.scrollHeight <= scroll.clientHeight + 1,
        'the card body scrolls with the page, not in a nested scroller',
      );
      assert(
        top(card.querySelector('.option')) <= innerHeight * 1.5,
        'the first option starts within one and a half screens',
      );
      const rows = new Map();
      card.querySelectorAll('.dock-group .btn').forEach((b) => {
        const y = Math.round(b.getBoundingClientRect().top);
        rows.set(y, (rows.get(y) || 0) + 1);
      });
      assert(new Set(rows.values()).size === 1, 'dock rows hold the same number of buttons');
      const small = [...document.querySelectorAll('header button, .card button, .card input')]
        .filter(visible)
        .filter((e) => {
          const b = e.getBoundingClientRect();
          return b.width < 44 || b.height < 44;
        });
      assert(!small.length, `controls are at least 44px (${small.map((e) => e.id || e.className)})`);
      const more = [...card.querySelectorAll('.more')].find((m) => visible(m) && m.closest('.block'));
      if (more) {
        const p = more.closest('.block').querySelector('.block-content > .md');
        more.click();
        assert(p.scrollHeight <= p.clientHeight + 1, 'Show more reveals the whole section');
        more.click();
        assert(p.scrollHeight > p.clientHeight + 1, 'Show less clamps the section again');
      }
      return { scenario: 'mobile-card', passed: true };
    },
    checkLongTitle = async () => {
      await waitFor(() => document.querySelector('.card h2.title'), 'the card title');
      const title = document.querySelector('.card h2.title'),
        more = document.querySelector('.card .title-more');
      assert(title.scrollHeight > title.clientHeight + 1, 'the seeded long title is clamped');
      assert(more && !more.hidden && visible(more), 'Show more appears for a clamped title');
      assert(
        document.documentElement.scrollWidth <= innerWidth,
        'a long title causes no horizontal overflow',
      );
      const dock = document.querySelector('.card .dock');
      assert(dock && dock.getBoundingClientRect().top < innerHeight, 'the dock stays on screen');
      more.click();
      assert(title.scrollHeight <= title.clientHeight + 1, 'Show more reveals the whole title');
      assert(/Show less/.test(more.textContent), 'the toggle flips to Show less');
      more.click();
      assert(title.scrollHeight > title.clientHeight + 1, 'Show less clamps the title again');
      const probe = document.createElement('div');
      probe.className = 'toast';
      probe.textContent = 'Skipped · ' + 'word '.repeat(200);
      document.querySelector('#toasts').append(probe);
      const clamped = probe.clientHeight < probe.scrollHeight - 1;
      const rect = probe.getBoundingClientRect();
      assert(rect.top > innerHeight / 2, 'a toast stays clear of the title at the top of a phone screen');
      probe.remove();
      assert(clamped, 'a long toast clamps instead of covering the screen');
      return { scenario: 'long-title', passed: true };
    };
  const checkPwa = async () => {
    await waitFor(() => document.querySelector('link[rel="manifest"]'), 'manifest link');
    assert(
      document.querySelector('link[rel="manifest"]')?.getAttribute('href') === 'manifest.webmanifest',
      'manifest link points to manifest.webmanifest',
    );
    assert(
      document.querySelector('link[rel="apple-touch-icon"]')?.getAttribute('href') === 'apple-touch-icon.png',
      'Apple touch icon points to apple-touch-icon.png',
    );
    assert('serviceWorker' in navigator, 'service workers are available');
    const registration = await navigator.serviceWorker.getRegistration();
    assert(registration, 'service worker registration exists');
    return { scenario: 'pwa', passed: true };
  };
  const checkHeader = async () => {
    const bar = document.querySelector('.bar');
    assert(visible(document.querySelector('#btn-settings')), 'the Settings button is visible');
    assert(document.documentElement.scrollWidth <= innerWidth, 'the page does not scroll sideways');
    assert(bar.scrollWidth <= bar.clientWidth, 'the header bar does not overflow');
    // Visually hidden labels are 1px wide on purpose; every other header item must show its whole content.
    for (const item of document.querySelectorAll('.bar > *, .top-actions > *')) {
      if (!visible(item) || item.getBoundingClientRect().width <= 1) continue;
      assert(
        item.scrollWidth <= item.clientWidth + 1,
        `${item.id || item.className} is not clipped (${item.scrollWidth} > ${item.clientWidth})`,
      );
    }
    return { scenario: 'header', passed: true };
  };

  // A card shortcut names an action, not a button slot: every dock button shows the key its
  // aria names, the same label keeps that key in whichever slot it sits, and the primary runs on
  // Enter beside its own key.
  const checkCardKeys = async () => {
    await waitFor(() => document.querySelector('.dock button'), 'a card with a dock');
    const fixed = {
        Acknowledge: 'a',
        'Acknowledge & close': 'x',
        'Plan next step': 'p',
        'Do next step': 'd',
        Dispatch: 'i',
        Skip: 's',
        Context: 't',
        Later: 'l',
        Peek: 'v',
        Jump: 'J',
        'Request final report': 'r',
        'Restore run': 'R',
      },
      label = (button) =>
        [...button.childNodes]
          .filter((node) => !(node.nodeType === 1 && node.matches('kbd.hint')))
          .map((node) => node.textContent)
          .join('')
          .replace(/\s+/g, ' ')
          .trim(),
      shown = [];
    for (const button of document.querySelectorAll('.dock button')) {
      const hint = button.querySelector('kbd.hint');
      if (!hint || !visible(hint)) continue;
      const name = label(button),
        key = hint.textContent.trim(),
        aria = button.getAttribute('aria-keyshortcuts');
      assert(name, 'every keyed dock button has a label');
      assert(aria, `${name} advertises the key it shows`);
      assert(aria.split(' ')[0] === key, `${name} advertises ${key} (${aria})`);
      if (fixed[name]) assert(key === fixed[name], `${name} keeps ${fixed[name]}, not ${key}`);
      if (button.hasAttribute('data-primary')) assert(aria === `${key} Enter`, `the primary ${name} runs on Enter beside ${key}`);
      shown.push(`${name}=${key}`);
    }
    if (innerWidth > 639 && matchMedia('(pointer: fine)').matches)
      assert(shown.length > 0, 'a dock button shows its action key on a pointer screen');
    return { scenario: 'card-keys', passed: true, keys: shown };
  };

  const checkDshCard = async () => {
    await waitFor(() => document.querySelector('#open-btn'), 'a dsh card with an Open button');
    assert(document.querySelector('#peek-btn, #peek-main'), 'dsh card offers Peek');
    assert(!document.querySelector('#jump-btn'), 'dsh card offers no Jump');
    if (document.querySelector('#peek-main')) {
      assert(
        [...document.querySelectorAll('.notice strong')].some((item) =>
          item.textContent.startsWith('Answer in the dsh web UI'),
        ),
        'read-only dsh card points to the dsh web UI',
      );
    }
    return { scenario: 'dsh-card', passed: true };
  };

  // Fakes the browser's speech engines to run the scenario in any browser. The first press uses the on-device
  // engine, the second one the engine Safari offers, which has neither available() nor processLocally. Both report
  // starting, as a real engine does, so the microphone shows that it listens.
  const checkDictation = async () => {
    const NativeRecognition = window.SpeechRecognition,
      settings = document.querySelector('#settings'),
      compose = document.querySelector('#compose'),
      prompt = document.querySelector('#compose-prompt'),
      mic = document.querySelector('[data-dictate="compose-prompt"]'),
      made = [],
      safari = [],
      // What both browsers hand back: array-like lists with no iterator, so spreading one throws.
      heard = (...transcripts) =>
        Object.assign({ length: transcripts.length }, ...transcripts.map((transcript, i) => ({ [i]: { 0: { transcript }, length: 1, isFinal: true } })));
    class FakeRecognition {
      static available = async () => 'available';
      static install = async () => true;
      constructor() {
        made.push(this);
      }
      start() {
        this.onstart();
      }
      stop() {
        this.onresult({ resultIndex: 0, results: heard('dictated words') });
        this.onend();
      }
      abort() {
        this.onend();
      }
    }
    class SafariRecognition {
      constructor() {
        safari.push(this);
      }
      start() {
        this.started = (this.started ?? 0) + 1;
        this.onstart();
      }
      stop() {
        this.onend();
      }
      abort() {
        this.onend();
      }
      end() {
        this.onend();
      }
    }
    FakeRecognition.prototype.processLocally = false;
    window.SpeechRecognition = FakeRecognition;
    try {
      document.querySelector('#btn-settings').click();
      await waitFor(
        () =>
          settings.open &&
          document.documentElement.dataset.dictation === 'on' &&
          document.querySelector('#compose-phrases [data-phrase]'),
        'the Settings sheet, dictation on and a saved phrase',
      );
      for (const id of ['#dictation-dictionary', '#dictation-phrases', '#dictation-save'])
        assert(visible(document.querySelector(id)), `${id} is visible`);
      assert(document.documentElement.scrollWidth <= innerWidth, 'the page does not scroll sideways');
      settings.close();
      const note = document.querySelector('#note');
      if (note) {
        const beside = document.querySelector('[data-dictate="note"]').getBoundingClientRect(),
          row = note.getBoundingClientRect();
        assert(beside.width && beside.top < row.bottom && row.top < beside.bottom, 'the note microphone sits beside the note');
      }
      document.querySelector('#btn-compose').click();
      await waitFor(() => compose.open, 'the New session sheet');
      assert(visible(mic), 'the prompt has a visible microphone');
      const phrase = document.querySelector('#compose-phrases [data-phrase]');
      phrase.click();
      assert(prompt.value === phrase.dataset.phrase, 'a phrase button writes its phrase into the prompt');
      prompt.value = 'Fix the build';
      prompt.setSelectionRange(prompt.value.length, prompt.value.length);
      mic.click();
      await waitFor(() => mic.getAttribute('aria-pressed') === 'true', 'the microphone to show it listens');
      assert(made.length === 1 && made[0].processLocally === true, 'the recognizer is told to stay on this device');
      mic.click();
      await waitFor(() => prompt.value === 'Fix the build dictated words', 'the dictated words in the prompt');
      await waitFor(() => mic.getAttribute('aria-pressed') === 'false', 'the microphone to reset');
      // Safari: only webkitSpeechRecognition, and no on-device recognition can be required.
      window.SpeechRecognition = undefined;
      window.webkitSpeechRecognition = SafariRecognition;
      document.querySelector('#btn-settings').click();
      await waitFor(
        () => settings.open && /speech service/.test(document.querySelector('#dictation-status').textContent),
        'the Settings sheet to name the browser speech service',
      );
      settings.close();
      assert(document.documentElement.dataset.dictation === 'on', 'Safari shows the microphones');
      prompt.value = 'Safari';
      prompt.setSelectionRange(prompt.value.length, prompt.value.length);
      mic.click();
      await waitFor(() => mic.getAttribute('aria-pressed') === 'true', 'the Safari microphone to show it listens');
      assert(safari.length === 1, 'the Safari recognizer starts on the press');
      assert(!('processLocally' in safari[0]), 'the Safari recognizer is not told to stay on this device');
      assert(safari[0].continuous === true, 'the Safari recognizer keeps listening');
      assert(safari[0].lang, 'the Safari recognizer is told the language');
      safari[0].onresult({ resultIndex: 0, results: heard('words') });
      await waitFor(() => prompt.value === 'Safari words', 'the Safari words in the prompt');
      safari[0].end();
      await waitFor(() => mic.getAttribute('aria-pressed') === 'false', 'Safari ending the run to reset the microphone');
      mic.click();
      await waitFor(() => safari[0].started === 2 && mic.getAttribute('aria-pressed') === 'true', 'the next press to reuse the Safari recognizer');
      assert(safari.length === 1, 'the next run reuses the Safari recognizer');
      mic.click();
      await waitFor(() => mic.getAttribute('aria-pressed') === 'false', 'the Safari microphone to stop on the press');
      return { scenario: 'dictation', passed: true };
    } finally {
      window.SpeechRecognition = NativeRecognition;
      delete window.webkitSpeechRecognition;
      prompt.value = '';
      prompt.dispatchEvent(new Event('input', { bubbles: true }));
      settings.close();
      compose.close();
    }
  };

  globalThis.checkTriage = async (scenario) => {
    if (scenario === 'peek') return checkPeek();
    if (scenario === 'dimensions') return checkDimensions();
    if (scenario === 'offline') return checkOffline();
    if (scenario === 'recovery') return checkRecovery();
    if (scenario === 'finding-other') return checkFindingOther();
    if (scenario === 'compose-stays') return checkComposeStaysOnQueue();
    if (scenario === 'picker') return checkPicker();
    if (scenario === 'models') return checkModelEngine();
    if (scenario === 'dispatch') return checkDispatchDraft();
    if (scenario === 'history-retry') return checkHistoryRetry();
    if (scenario === 'pwa') return checkPwa();
    if (scenario === 'portrait') return checkPortrait();
    if (scenario === 'landscape') return checkLandscape();
    if (scenario === 'mobile-card') return checkMobileCard();
    if (scenario === 'long-title') return checkLongTitle();
    if (scenario === 'plan-into') return checkPlanInto();
    if (scenario === 'plan-approval') return checkPlanApproval();
    if (scenario === 'header') return checkHeader();
    if (scenario === 'card-keys') return checkCardKeys();
    if (scenario === 'dsh-card') return checkDshCard();
    if (scenario === 'dictation') return checkDictation();
    fail(`unknown scenario ${String(scenario)}`);
  };
})();
