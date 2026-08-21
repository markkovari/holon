import React, { useState } from 'react';
import { BrowserRouter, Routes, Route, Navigate } from 'react-router-dom';
import { Wallet, LogIn, PieChart, PlusCircle, ArrowUpCircle, ArrowDownCircle, LogOut } from 'lucide-react';

const MockLogin = ({ onLogin }) => {
  const [username, setUsername] = useState('');
  
  const handleLogin = (e) => {
    e.preventDefault();
    if (username.trim()) {
      onLogin(username);
    }
  };

  return (
    <div className="min-h-screen bg-gray-100 flex items-center justify-center">
      <div className="bg-white p-8 rounded-lg shadow-md w-96">
        <div className="flex items-center justify-center mb-8 text-indigo-600">
          <Wallet size={48} />
        </div>
        <h1 className="text-2xl font-bold text-center text-gray-800 mb-6">Budget Tracker</h1>
        <form onSubmit={handleLogin} className="space-y-4">
          <div>
            <label className="block text-sm font-medium text-gray-700 mb-1">Username</label>
            <input 
              type="text" 
              className="w-full px-4 py-2 border border-gray-300 rounded-md focus:outline-none focus:ring-2 focus:ring-indigo-500"
              placeholder="Enter any username"
              value={username}
              onChange={(e) => setUsername(e.target.value)}
            />
          </div>
          <button 
            type="submit" 
            className="w-full bg-indigo-600 text-white py-2 rounded-md hover:bg-indigo-700 flex items-center justify-center gap-2 transition-colors"
          >
            <LogIn size={20} />
            Sign In
          </button>
        </form>
      </div>
    </div>
  );
};

const Dashboard = ({ token, onLogout }) => {
  // In a real app, this would fetch from the backend using the token
  const [balance, setBalance] = useState(1250.00);
  const [transactions, setTransactions] = useState([
    { id: 1, amount: 50.00, category: 'Groceries', type: 'expense', date: '2023-10-01' },
    { id: 2, amount: 2000.00, category: 'Salary', type: 'income', date: '2023-10-01' },
  ]);
  const [amount, setAmount] = useState('');
  const [category, setCategory] = useState('');
  const [type, setType] = useState('expense');

  const handleAddTransaction = (e) => {
    e.preventDefault();
    if (!amount || !category) return;
    
    const newTransaction = {
      id: Date.now(),
      amount: parseFloat(amount),
      category,
      type,
      date: new Date().toISOString().split('T')[0]
    };
    
    setTransactions([newTransaction, ...transactions]);
    setBalance(prev => type === 'income' ? prev + newTransaction.amount : prev - newTransaction.amount);
    
    setAmount('');
    setCategory('');
  };

  return (
    <div className="min-h-screen bg-gray-50">
      <nav className="bg-indigo-600 text-white p-4 shadow-md">
        <div className="max-w-6xl mx-auto flex justify-between items-center">
          <div className="flex items-center gap-2 text-xl font-bold">
            <Wallet />
            Budget Tracker
          </div>
          <div className="flex items-center gap-4">
            <span className="text-sm bg-indigo-800 px-3 py-1 rounded-full">{token}</span>
            <button onClick={onLogout} className="p-2 hover:bg-indigo-700 rounded-full transition-colors">
              <LogOut size={20} />
            </button>
          </div>
        </div>
      </nav>

      <main className="max-w-6xl mx-auto p-6 grid grid-cols-1 md:grid-cols-3 gap-6">
        {/* Left Column - Balance & Add Transaction */}
        <div className="space-y-6">
          <div className="bg-white p-6 rounded-xl shadow-sm border border-gray-100 text-center">
            <h2 className="text-gray-500 font-medium mb-2">Total Balance</h2>
            <div className="text-4xl font-bold text-gray-800">${balance.toFixed(2)}</div>
          </div>

          <div className="bg-white p-6 rounded-xl shadow-sm border border-gray-100">
            <h2 className="text-lg font-bold text-gray-800 mb-4 flex items-center gap-2">
              <PlusCircle size={20} className="text-indigo-600"/>
              New Transaction
            </h2>
            <form onSubmit={handleAddTransaction} className="space-y-4">
              <div className="flex gap-2">
                <button
                  type="button"
                  onClick={() => setType('expense')}
                  className={`flex-1 py-2 rounded-md font-medium text-sm flex items-center justify-center gap-1 ${type === 'expense' ? 'bg-red-100 text-red-700 border border-red-200' : 'bg-gray-100 text-gray-600'}`}
                >
                  <ArrowDownCircle size={16} /> Expense
                </button>
                <button
                  type="button"
                  onClick={() => setType('income')}
                  className={`flex-1 py-2 rounded-md font-medium text-sm flex items-center justify-center gap-1 ${type === 'income' ? 'bg-green-100 text-green-700 border border-green-200' : 'bg-gray-100 text-gray-600'}`}
                >
                  <ArrowUpCircle size={16} /> Income
                </button>
              </div>
              
              <div>
                <label className="block text-sm text-gray-600 mb-1">Amount</label>
                <input 
                  type="number" 
                  step="0.01"
                  className="w-full px-3 py-2 border border-gray-200 rounded-md focus:outline-none focus:border-indigo-500"
                  value={amount}
                  onChange={(e) => setAmount(e.target.value)}
                  placeholder="0.00"
                  required
                />
              </div>
              
              <div>
                <label className="block text-sm text-gray-600 mb-1">Category</label>
                <input 
                  type="text" 
                  className="w-full px-3 py-2 border border-gray-200 rounded-md focus:outline-none focus:border-indigo-500"
                  value={category}
                  onChange={(e) => setCategory(e.target.value)}
                  placeholder="e.g. Groceries"
                  required
                />
              </div>

              <button type="submit" className="w-full bg-indigo-600 text-white py-2 rounded-md hover:bg-indigo-700 font-medium">
                Add {type === 'income' ? 'Income' : 'Expense'}
              </button>
            </form>
          </div>
        </div>

        {/* Right Column - Transactions List */}
        <div className="md:col-span-2">
          <div className="bg-white p-6 rounded-xl shadow-sm border border-gray-100 h-full">
            <h2 className="text-lg font-bold text-gray-800 mb-6 flex items-center gap-2">
              <PieChart size={20} className="text-indigo-600" />
              Recent Transactions
            </h2>
            
            <div className="space-y-4">
              {transactions.length === 0 ? (
                <div className="text-center text-gray-500 py-8">No transactions yet.</div>
              ) : (
                transactions.map(t => (
                  <div key={t.id} className="flex justify-between items-center p-4 border border-gray-100 rounded-lg hover:shadow-sm transition-shadow bg-gray-50">
                    <div className="flex items-center gap-4">
                      <div className={`p-2 rounded-full ${t.type === 'income' ? 'bg-green-100 text-green-600' : 'bg-red-100 text-red-600'}`}>
                        {t.type === 'income' ? <ArrowUpCircle size={24} /> : <ArrowDownCircle size={24} />}
                      </div>
                      <div>
                        <div className="font-semibold text-gray-800">{t.category}</div>
                        <div className="text-sm text-gray-500">{t.date}</div>
                      </div>
                    </div>
                    <div className={`font-bold text-lg ${t.type === 'income' ? 'text-green-600' : 'text-gray-800'}`}>
                      {t.type === 'income' ? '+' : '-'}${t.amount.toFixed(2)}
                    </div>
                  </div>
                ))
              )}
            </div>
          </div>
        </div>
      </main>
    </div>
  );
};

export default function App() {
  const [token, setToken] = useState(null);

  return (
    <BrowserRouter>
      <Routes>
        <Route 
          path="/login" 
          element={!token ? <MockLogin onLogin={setToken} /> : <Navigate to="/" />} 
        />
        <Route 
          path="/" 
          element={token ? <Dashboard token={token} onLogout={() => setToken(null)} /> : <Navigate to="/login" />} 
        />
      </Routes>
    </BrowserRouter>
  );
}
