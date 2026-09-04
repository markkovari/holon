import { useGrocery } from "./hooks/useGrocery";
import { Header } from "./components/common/Header";
import { Toast } from "./components/common/Toast";
import { AuthModal } from "./components/common/AuthModal";
import { ShopperView } from "./components/shopper/ShopperView";
import { AdminView } from "./components/admin/AdminView";
import { CartModal } from "./components/shopper/CartModal";
import { OrderReceiptModal } from "./components/shopper/OrderReceiptModal";

export default function App() {
  const grocery = useGrocery();
  const isAdmin = grocery.currentUser?.role === "admin";

  return (
    <div className="mobile-shell">
      {/* Top Application Bar with Real Identity */}
      <Header
        currentUser={grocery.currentUser}
        adminViewMode={grocery.adminViewMode}
        onSetAdminViewMode={grocery.setAdminViewMode}
        onSync={grocery.loadProducts}
        cartItemCount={grocery.cartItemCount}
        cartTotalCents={grocery.cartTotalCents}
        onOpenCart={() => grocery.setIsCartOpen(true)}
        onOpenAuth={grocery.openAuthModal}
        onLogout={grocery.handleLogout}
        loading={grocery.loading}
      />

      <main className="app-main">
        {isAdmin && grocery.adminViewMode === "console" ? (
          <AdminView
            products={grocery.products}
            lowStockProducts={grocery.lowStockProducts}
            scanning={grocery.scanning}
            onAdjustStock={grocery.adjustStock}
            onFileUpload={grocery.handleFileUpload}
            onTestFixture={grocery.handleTestFixture}
            newBarcode={grocery.newBarcode}
            newSymbology={grocery.newSymbology}
            newName={grocery.newName}
            newCategory={grocery.newCategory}
            newPrice={grocery.newPrice}
            newStock={grocery.newStock}
            onBarcodeChange={grocery.setNewBarcode}
            onSymbologyChange={grocery.setNewSymbology}
            onNameChange={grocery.setNewName}
            onCategoryChange={grocery.setNewCategory}
            onPriceChange={grocery.setNewPrice}
            onStockChange={grocery.setNewStock}
            onRegisterSku={grocery.handleRegisterSku}
            adminUsers={grocery.adminUsers}
            currentUserId={grocery.currentUser?.id}
            onUpdateUserRole={grocery.handleUpdateUserRole}
            onDeleteUser={grocery.handleDeleteUser}
            onCreateUser={grocery.handleAdminCreateUser}
            onRefreshUsers={grocery.loadAdminUsers}
          />
        ) : (
          <ShopperView
            products={grocery.products}
            scanning={grocery.scanning}
            scanResult={grocery.scanResult}
            scanError={grocery.scanError}
            onFileUpload={grocery.handleFileUpload}
            onTestFixture={grocery.handleTestFixture}
            onAddToCart={grocery.addToCart}
            cartTotalCents={grocery.cartTotalCents}
            cartItemCount={grocery.cartItemCount}
            onOpenCart={() => grocery.setIsCartOpen(true)}
          />
        )}
      </main>

      {/* Identity & Real Authentication Modal */}
      <AuthModal
        isOpen={grocery.authModalOpen}
        initialTab={grocery.authModalTab}
        onClose={() => grocery.setAuthModalOpen(false)}
        onLogin={grocery.handleLogin}
        onRegister={grocery.handleRegister}
      />

      {/* Shopping Basket Modal */}
      <CartModal
        isOpen={grocery.isCartOpen}
        onClose={() => grocery.setIsCartOpen(false)}
        cart={grocery.cart}
        onUpdateQty={grocery.updateCartQty}
        onRemove={grocery.removeFromCart}
        cartTotalCents={grocery.cartTotalCents}
        onCheckout={grocery.handleCheckout}
        loading={grocery.loading}
      />

      {/* Checkout Receipt Modal */}
      <OrderReceiptModal
        receipt={grocery.checkoutReceipt}
        onDismiss={() => grocery.setCheckoutReceipt(null)}
      />

      {/* Toast Notification */}
      <Toast message={grocery.toastMessage} />
    </div>
  );
}
