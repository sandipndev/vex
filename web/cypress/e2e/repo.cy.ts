// Repository listing e2e tests against a Docker vexd container.
//
// Expects:
//   - vexd running in container "vex-web-test" with HTTP port 9423 mapped to 7423
//   - Next.js dev server on localhost:3000

const VEX_HOST = "localhost:9423";

function pairToken(): Cypress.Chainable<string> {
  return cy
    .exec("docker exec vex-web-test vexd pair")
    .then((result) => {
      const match = result.stdout.match(/tok_[a-f0-9]+:[a-f0-9]+/);
      expect(match).to.not.be.null;
      return match![0];
    });
}

function fillAndConnect(host: string, pairing: string) {
  cy.get("[data-cy=host-input]").clear().type(host);
  cy.get("[data-cy=pairing-input]").clear().type(pairing);
  cy.get("[data-cy=connect-button]").click();
}

describe("Repository listing", () => {
  beforeEach(() => {
    cy.visit("/app");
    cy.window().then((win) => win.localStorage.clear());
    cy.reload();
  });

  it("shows registered repo in web UI", () => {
    // Register a repo inside the container
    cy.exec("docker exec vex-web-test mkdir -p /tmp/test-repo");
    cy.exec(
      "docker exec vex-web-test vexd repo register test-repo /tmp/test-repo"
    );

    pairToken().then((pairing) => {
      fillAndConnect(VEX_HOST, pairing);
      cy.get("[data-cy=status-version]", { timeout: 10000 }).should(
        "contain",
        "vexd v"
      );
      cy.get("[data-cy=repos-section]").should("exist");
      cy.get("[data-cy=repo-item]").should("contain", "test-repo");
    });
  });

  it("shows empty state when no repos registered", () => {
    pairToken().then((pairing) => {
      fillAndConnect(VEX_HOST, pairing);
      cy.get("[data-cy=status-version]", { timeout: 10000 }).should(
        "contain",
        "vexd v"
      );
      cy.get("[data-cy=repos-section]").should("exist");
      cy.get("[data-cy=repos-empty]").should(
        "contain",
        "No repositories registered"
      );
    });
  });
});
