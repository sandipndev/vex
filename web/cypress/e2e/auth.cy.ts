// Auth flow e2e tests against a Docker vexd container.
//
// Expects:
//   - vexd running in container "vex-web-test" with port 9422 mapped to 7422
//   - Next.js dev server on localhost:3000

const VEX_HOST = "localhost:9422";
const ZERO_SECRET =
  "0000000000000000000000000000000000000000000000000000000000000000";

function pairToken(): Cypress.Chainable<{ tokenId: string; tokenSecret: string }> {
  return cy
    .exec("docker exec vex-web-test vexd pair")
    .then((result) => {
      const match = result.stdout.match(/tok_[a-f0-9]+:[a-f0-9]+/);
      expect(match).to.not.be.null;
      const [tokenId, tokenSecret] = match![0].split(":");
      return { tokenId, tokenSecret };
    });
}

function fillAndConnect(host: string, tokenId: string, tokenSecret: string) {
  cy.get("[data-cy=host-input]").clear().type(host);
  cy.get("[data-cy=token-id-input]").clear().type(tokenId);
  cy.get("[data-cy=token-secret-input]").clear().type(tokenSecret);
  cy.get("[data-cy=connect-button]").click();
}

describe("Auth flow", () => {
  beforeEach(() => {
    cy.visit("/app");
    // Clear localStorage to start fresh
    cy.window().then((win) => win.localStorage.clear());
    cy.reload();
  });

  it("connects with valid token and shows status", () => {
    pairToken().then(({ tokenId, tokenSecret }) => {
      fillAndConnect(VEX_HOST, tokenId, tokenSecret);
      cy.get("[data-cy=status-version]", { timeout: 10000 }).should("contain", "vexd v");
      cy.get("[data-cy=status-uptime]").should("exist");
      cy.get("[data-cy=status-clients]").should("exist");
    });
  });

  it("rejects fabricated token", () => {
    fillAndConnect(VEX_HOST, "tok_000000", ZERO_SECRET);
    cy.get("[data-cy=error-message]", { timeout: 10000 }).should("exist");
  });

  it("rejects wrong secret for valid token ID", () => {
    pairToken().then(({ tokenId }) => {
      fillAndConnect(VEX_HOST, tokenId, ZERO_SECRET);
      cy.get("[data-cy=error-message]", { timeout: 10000 }).should("exist");
    });
  });

  it("disconnect and reconnect", () => {
    pairToken().then(({ tokenId, tokenSecret }) => {
      fillAndConnect(VEX_HOST, tokenId, tokenSecret);
      cy.get("[data-cy=status-version]", { timeout: 10000 }).should("contain", "vexd v");

      // Disconnect
      cy.get("[data-cy=disconnect-button]").click();
      cy.get("[data-cy=connect-button]").should("exist");

      // Reconnect
      fillAndConnect(VEX_HOST, tokenId, tokenSecret);
      cy.get("[data-cy=status-version]", { timeout: 10000 }).should("contain", "vexd v");
    });
  });
});
