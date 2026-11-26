const { assertNoStubError, textOf, waitForPendingCount, waitForStubReady } = require('../support/waits');

// The harness's #run control cannot pass ToolCallOptions, so this calls the bridge directly.
describe('tool approval events carry the conversation', () => {
    it('forwards the conversation id named on the call as conversationId', async () => {
        await waitForStubReady();
        const conversationId = `e2e-conversation-${Date.now()}`;

        await browser.execute((id) => {
            window.__e2eConversationApprovals = [];
            window.__e2eConversationResult = null;
            window.__e2eConversationListener = (event) => window.__e2eConversationApprovals.push(event.detail);
            window.addEventListener('lumo:tool-approval-request', window.__e2eConversationListener);
            // Not awaited: the call stays pending until the approval below is answered.
            window.LumoConnectors.executeToolCall(
                { id: 'e2e-conversation-call', name: 'echo__ask', arguments: {} },
                undefined,
                { conversationId: id },
            ).then(
                (result) => { window.__e2eConversationResult = result; },
                (error) => { window.__e2eConversationResult = { is_error: true, content: String(error) }; },
            );
        }, conversationId);

        try {
            await waitForPendingCount(1);
            const requestId = await textOf('#pending-id');

            let approval;
            await browser.waitUntil(async () => {
                await assertNoStubError();
                approval = await browser.execute((id) => (
                    window.__e2eConversationApprovals.find((detail) => detail.requestId === id) ?? null
                ), requestId);
                return approval !== null;
            }, { timeout: 20_000, timeoutMsg: `approval event for ${requestId} never arrived` });

            expect(approval.conversationId).toBe(conversationId);

            await $('#approve').click();
            let result;
            await browser.waitUntil(async () => {
                result = await browser.execute(() => window.__e2eConversationResult);
                return result !== null;
            }, { timeout: 20_000, timeoutMsg: 'approved call never completed' });
            expect(result.is_error).toBeFalsy();
            await waitForPendingCount(0);
        } finally {
            await browser.execute(() => {
                window.removeEventListener('lumo:tool-approval-request', window.__e2eConversationListener);
                delete window.__e2eConversationListener;
                delete window.__e2eConversationApprovals;
                delete window.__e2eConversationResult;
            });
        }
    });
});
