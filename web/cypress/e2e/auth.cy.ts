// Auth flow e2e tests against a Docker vexd container.
//
// Expects:
//   - vexd running in container "vex-web-test" with HTTP port 9423 mapped to 7423
//   - Next.js dev server on localhost:3000

const VEX_HOST = "localhost:9423";
const ZERO_SECRET =
  "0000000000000000000000000000000000000000000000000000000000000000";

function pairToken(): Cypress.Chainable<string> {
  return cy
    .exec("docker exec vex-web-test vexd pair")
    .then((result) => {
      const match = result.stdout.match(/tok_[a-f0-9]+:[a-f0-9]+/);
      expect(match).to.not.be.null;
      return match![0]; // full pairing string: tok_id:secret
    });
}

function fillAndConnect(host: string, pairing: string) {
  cy.get("[data-cy=host-input]").clear().type(host);
  cy.get("[data-cy=pairing-input]").clear().type(pairing);
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
    pairToken().then((pairing) => {
      fillAndConnect(VEX_HOST, pairing);
      cy.get("[data-cy=status-version]", { timeout: 10000 }).should("contain", "vexd v");
      cy.get("[data-cy=status-uptime]").should("exist");
      cy.get("[data-cy=status-clients]").should("exist");
    });
  });

  it("rejects fabricated token", () => {
    fillAndConnect(VEX_HOST, `tok_000000:${ZERO_SECRET}`);
    cy.get("[data-cy=error-message]", { timeout: 10000 }).should("exist");
  });

  it("rejects wrong secret for valid token ID", () => {
    pairToken().then((pairing) => {
      const tokenId = pairing.split(":")[0];
      fillAndConnect(VEX_HOST, `${tokenId}:${ZERO_SECRET}`);
      cy.get("[data-cy=error-message]", { timeout: 10000 }).should("exist");
    });
  });

  it("disconnect and reconnect", () => {
    pairToken().then((pairing) => {
      fillAndConnect(VEX_HOST, pairing);
      cy.get("[data-cy=status-version]", { timeout: 10000 }).should("contain", "vexd v");

      // Disconnect
      cy.get("[data-cy=disconnect-button]").click();
      cy.get("[data-cy=connect-button]").should("exist");

      // Reconnect
      fillAndConnect(VEX_HOST, pairing);
      cy.get("[data-cy=status-version]", { timeout: 10000 }).should("contain", "vexd v");
    });
  });
});
