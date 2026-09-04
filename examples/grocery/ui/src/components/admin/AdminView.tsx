import { useState } from "react";
import type React from "react";
import { Layers, Users } from "lucide-react";
import { Product, User, Role, RegisterPayload } from "../../types/grocery";
import { KpiMetrics } from "./KpiMetrics";
import { LowStockAlerts } from "./LowStockAlerts";
import { IntakeScanner } from "./IntakeScanner";
import { SkuRegistration } from "./SkuRegistration";
import { InventoryList } from "./InventoryList";
import { UserManagement } from "./UserManagement";

interface AdminViewProps {
  products: Product[];
  lowStockProducts: Product[];
  scanning: boolean;
  onAdjustStock: (barcode: string, delta: number) => void;
  onFileUpload: (e: React.ChangeEvent<HTMLInputElement>) => void;
  onTestFixture: (filename: string) => void;
  newBarcode: string;
  newSymbology: string;
  newName: string;
  newCategory: string;
  newPrice: string;
  newStock: string;
  onBarcodeChange: (val: string) => void;
  onSymbologyChange: (val: string) => void;
  onNameChange: (val: string) => void;
  onCategoryChange: (val: string) => void;
  onPriceChange: (val: string) => void;
  onStockChange: (val: string) => void;
  onRegisterSku: (e: React.FormEvent) => void;
  // User Management
  adminUsers: User[];
  currentUserId?: string;
  onUpdateUserRole: (userId: string, newRole: Role) => Promise<void>;
  onDeleteUser: (userId: string) => Promise<void>;
  onCreateUser: (payload: RegisterPayload) => Promise<void>;
  onRefreshUsers: () => Promise<void>;
}

export const AdminView: React.FC<AdminViewProps> = ({
  products,
  lowStockProducts,
  scanning,
  onAdjustStock,
  onFileUpload,
  onTestFixture,
  newBarcode,
  newSymbology,
  newName,
  newCategory,
  newPrice,
  newStock,
  onBarcodeChange,
  onSymbologyChange,
  onNameChange,
  onCategoryChange,
  onPriceChange,
  onStockChange,
  onRegisterSku,
  adminUsers,
  currentUserId,
  onUpdateUserRole,
  onDeleteUser,
  onCreateUser,
  onRefreshUsers,
}) => {
  const [adminTab, setAdminTab] = useState<"inventory" | "users">("inventory");
  const totalUnits = products.reduce((acc, p) => acc + p.stock, 0);

  return (
    <div className="admin-container animate-fade-in">
      {/* High-level KPI metric strip */}
      <KpiMetrics
        productCount={products.length}
        lowStockCount={lowStockProducts.length}
        totalUnits={totalUnits}
      />

      {/* Admin Sub-navigation: Inventory vs Users */}
      <div
        style={{
          display: "flex",
          gap: "0.5rem",
          marginBottom: "1rem",
        }}
      >
        <button
          id="admin-tab-inventory"
          className={`chip-btn ${adminTab === "inventory" ? "active" : ""}`}
          style={{
            display: "flex",
            alignItems: "center",
            gap: "0.4rem",
            padding: "0.45rem 0.9rem",
            fontSize: "0.82rem",
            fontWeight: 600,
          }}
          onClick={() => setAdminTab("inventory")}
        >
          <Layers size={15} />
          <span>Catalog & Inventory</span>
        </button>
        <button
          id="admin-tab-users"
          className={`chip-btn ${adminTab === "users" ? "active" : ""}`}
          style={{
            display: "flex",
            alignItems: "center",
            gap: "0.4rem",
            padding: "0.45rem 0.9rem",
            fontSize: "0.82rem",
            fontWeight: 600,
          }}
          onClick={() => {
            setAdminTab("users");
            onRefreshUsers();
          }}
        >
          <Users size={15} />
          <span>User Management ({adminUsers.length})</span>
        </button>
      </div>

      {adminTab === "inventory" ? (
        <div className="admin-layout">
          {/* Left Column: Alerts, Scanner, SKU Form */}
          <div className="admin-left-pane">
            <LowStockAlerts
              lowStockProducts={lowStockProducts}
              onAdjustStock={onAdjustStock}
            />

            <IntakeScanner
              scanning={scanning}
              onFileUpload={onFileUpload}
              onTestFixture={onTestFixture}
            />

            <SkuRegistration
              newBarcode={newBarcode}
              newSymbology={newSymbology}
              newName={newName}
              newCategory={newCategory}
              newPrice={newPrice}
              newStock={newStock}
              onBarcodeChange={onBarcodeChange}
              onSymbologyChange={onSymbologyChange}
              onNameChange={onNameChange}
              onCategoryChange={onCategoryChange}
              onPriceChange={onPriceChange}
              onStockChange={onStockChange}
              onSubmit={onRegisterSku}
            />
          </div>

          {/* Right Column: Real-Time Stock Inventory */}
          <div className="admin-right-pane">
            <InventoryList
              products={products}
              onAdjustStock={onAdjustStock}
            />
          </div>
        </div>
      ) : (
        <UserManagement
          users={adminUsers}
          currentUserId={currentUserId}
          onUpdateRole={onUpdateUserRole}
          onDeleteUser={onDeleteUser}
          onCreateUser={onCreateUser}
          onRefresh={onRefreshUsers}
        />
      )}
    </div>
  );
};
