import SwiftUI

// PLACEHOLDERS (shell branch only). The Books agent's `BooksScreen`,
// book details and `BookReaderScreen` replace these at merge: swap the three
// references in RootView.swift (`PhoneRoot`, `PadRoot.detail`, `RouteView`)
// and delete this file. The routes they hang off are `AppTab.books`,
// `PadItem.books`, `Route.book(id)` and `Route.bookReader(id)`
// (`router.showBooks()`, `router.readBook(id)`).

/// The Books tab / sidebar destination until the Books screens land.
struct BooksPlaceholder: View {
    var body: some View {
        EmptyState(symbol: "books.vertical", title: "Books", message: "Your library of books arrives with the Books screens.")
            .frame(maxWidth: .infinity, maxHeight: .infinity)
            .groundBackground()
            .navigationTitle("Books")
    }
}

/// A book's details until the Books screens land.
struct BookPlaceholder: View {
    let id: String

    var body: some View {
        EmptyState(symbol: "book.closed", title: "Book", message: "Book details arrive with the Books screens.")
            .frame(maxWidth: .infinity, maxHeight: .infinity)
            .groundBackground()
    }
}

/// The reader until the Books screens land.
struct BookReaderPlaceholder: View {
    let id: String

    var body: some View {
        EmptyState(symbol: "book", title: "Reader", message: "Reading in the app arrives with the Books screens.")
            .frame(maxWidth: .infinity, maxHeight: .infinity)
            .groundBackground()
    }
}
