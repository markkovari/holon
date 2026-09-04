import React from "react";
import { CheckCircle2 } from "lucide-react";
import { OrderReceipt } from "../../types/grocery";

interface OrderReceiptModalProps {
  receipt: OrderReceipt | null;
  onDismiss: () => void;
}

export const OrderReceiptModal: React.FC<OrderReceiptModalProps> = ({
  receipt,
  onDismiss,
}) => {
  if (!receipt) return null;

  return (
    <div className="mobile-modal-overlay" onClick={onDismiss}>
      <div
        className="mobile-bottom-sheet modal-content"
        style={{ textAlign: "center" }}
        onClick={(e) => e.stopPropagation()}
      >
        <CheckCircle2 size={44} style={{ color: "#34d399", margin: "0 auto 0.75rem auto" }} />
        <h3 style={{ fontSize: "1.2rem", fontWeight: 700, marginBottom: "0.35rem" }}>
          Payment Successful!
        </h3>
        <p style={{ color: "var(--text-secondary)", fontSize: "0.8rem", marginBottom: "1.2rem" }}>
          Inventory stock decremented in real-time in WASI.
        </p>

        <div
          style={{
            background: "rgba(30, 41, 59, 0.5)",
            borderRadius: "12px",
            padding: "0.9rem",
            textAlign: "left",
            marginBottom: "1.25rem",
            border: "1px solid var(--border)",
          }}
        >
          <div
            style={{
              display: "flex",
              justifyContent: "space-between",
              marginBottom: "0.4rem",
              fontSize: "0.8rem",
            }}
          >
            <span style={{ color: "var(--text-muted)" }}>Order Reference:</span>
            <span className="mono" style={{ fontWeight: 600 }}>
              {receipt.orderId}
            </span>
          </div>
          <div
            style={{
              display: "flex",
              justifyContent: "space-between",
              marginBottom: "0.4rem",
              fontSize: "0.8rem",
            }}
          >
            <span style={{ color: "var(--text-muted)" }}>Items Purchased:</span>
            <span>{receipt.itemsCount} items</span>
          </div>
          <div
            style={{
              display: "flex",
              justifyContent: "space-between",
              fontWeight: 700,
              fontSize: "0.95rem",
              borderTop: "1px solid var(--border)",
              paddingTop: "0.4rem",
            }}
          >
            <span>Amount Paid:</span>
            <span style={{ color: "#34d399" }}>
              ${((receipt.total * 1.08) / 100).toFixed(2)}
            </span>
          </div>
        </div>

        <button
          id="dismiss-receipt-btn"
          className="btn-mobile btn-mobile-primary"
          style={{ width: "100%", padding: "0.75rem" }}
          onClick={onDismiss}
        >
          Done Shopping
        </button>
      </div>
    </div>
  );
};
