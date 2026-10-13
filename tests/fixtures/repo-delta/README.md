# Delta

Invoicing for small subscription businesses: an invoice ledger, billing cycles with late fees, statement and
direct debit exports, and sign-in for the customer portal.

## Build and test
```
dotnet build Delta.sln
dotnet test tests/Invoicing.Tests tests/Export.Tests
```

## Releases
1.4.0 added invoice filters to the ledger and faster statement exports.
