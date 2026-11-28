# portald

Customers sign in to see their charging agreements, meter receipts and issued UBL invoices. Each agreement links to its archived price and shows the status of a recorded withdrawal. Private payers and fleet billing delegates can download documents shared with their account, including credits and replacements. A driver grant opens only that driver’s agreements and receipts. When the provider erases an identity’s grants, its current sessions and sign-in credential immediately lose access.

Set `[portal].public_url` to the customer-facing origin. Use HTTPS on a dedicated hostname for deployment. For a local estate use `http://127.0.0.1:9589`. The listener uses `PORTALD_HTTP_BIND` and defaults to `127.0.0.1:9589`. The portal contacts `[services].empd` for identity, `[services].billd` for invoices and the configured `[services].accountingd` for balances. Charging receipts and archived prices use the configured `[receipts].public_url`.

Open `/` to sign in. Choose English or Deutsch on the page. Each form submits to `/customer/` on the configured origin. The session cookie stays on that path, uses HttpOnly and SameSite=Strict, and is Secure on HTTPS. Staff credentials and driver cards cannot sign in. A download retrieves the issued document from billing. **View invoice balance** shows the original gross amount, line credits, allocated payments and the amount due or in credit. When finance applies credit between invoices, the payer sees how much credit this invoice received or contributed. The portal checks access again each time the payer opens a balance. Configure `services.accountingd` to enable balances.

The provider owns sign-in, grants and logout. Billing checks the current grants on every history request and download. If billing is unavailable, the customer can still inspect agreements. If the provider is unavailable, local pages show a retry action and withhold customer data.

Run `cargo test -p portald` for the browser boundary checks and `just e2e` for the standalone owner workflow.

Open **View invoice balance** on an invoice and choose **Back to overview** to return to the same history page and language. The document number travels in the submitted form.
