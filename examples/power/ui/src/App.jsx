import React, { useState } from 'react';
import { Calculator, LogOut, Zap } from 'lucide-react';

function App() {
  const [token, setToken] = useState(null);
  const [wattage, setWattage] = useState('');
  const [hours, setHours] = useState('');
  const [cost, setCost] = useState(null);

  const handleLogin = (e) => {
    e.preventDefault();
    setToken('mock-jwt-token-12345');
  };

  const handleLogout = () => {
    setToken(null);
    setCost(null);
    setWattage('');
    setHours('');
  };

  const handleCalculate = async () => {
    if (!token) return;
    
    // Simulating backend call with token validation
    // In a real app, this would be an HTTP request to the backend passing the token
    const kwh = (parseFloat(wattage || 0) * parseFloat(hours || 0)) / 1000;
    setCost(kwh * 0.15); // The backend would calculate this
  };

  if (!token) {
    return (
      <div className="min-h-screen bg-gray-100 flex items-center justify-center p-4">
        <div className="bg-white rounded-xl shadow-lg p-8 max-w-md w-full">
          <div className="flex justify-center mb-6">
            <div className="p-3 bg-blue-100 rounded-full">
              <Zap className="w-8 h-8 text-blue-600" />
            </div>
          </div>
          <h1 className="text-2xl font-bold text-center text-gray-800 mb-8">Power Calculator</h1>
          <form onSubmit={handleLogin} className="space-y-4">
            <div>
              <label className="block text-sm font-medium text-gray-700 mb-1">Email</label>
              <input 
                type="email" 
                className="w-full px-4 py-2 border border-gray-300 rounded-lg focus:ring-2 focus:ring-blue-500 focus:border-blue-500 outline-none"
                placeholder="user@example.com"
                required
              />
            </div>
            <div>
              <label className="block text-sm font-medium text-gray-700 mb-1">Password</label>
              <input 
                type="password" 
                className="w-full px-4 py-2 border border-gray-300 rounded-lg focus:ring-2 focus:ring-blue-500 focus:border-blue-500 outline-none"
                placeholder="••••••••"
                required
              />
            </div>
            <button 
              type="submit" 
              className="w-full bg-blue-600 text-white font-semibold py-2 px-4 rounded-lg hover:bg-blue-700 transition duration-200"
              data-testid="login-button"
            >
              Sign In
            </button>
          </form>
        </div>
      </div>
    );
  }

  return (
    <div className="min-h-screen bg-gray-100 p-4">
      <div className="max-w-2xl mx-auto">
        <div className="bg-white rounded-xl shadow-lg overflow-hidden">
          {/* Header */}
          <div className="bg-blue-600 p-6 flex justify-between items-center">
            <div className="flex items-center space-x-3">
              <Zap className="w-6 h-6 text-white" />
              <h1 className="text-xl font-bold text-white">Power Calculator</h1>
            </div>
            <button 
              onClick={handleLogout}
              className="text-blue-100 hover:text-white transition flex items-center space-x-2 text-sm font-medium"
            >
              <LogOut className="w-4 h-4" />
              <span>Sign Out</span>
            </button>
          </div>

          {/* Body */}
          <div className="p-6 md:p-8">
            <div className="grid grid-cols-1 md:grid-cols-2 gap-6">
              <div className="space-y-4">
                <div>
                  <label className="block text-sm font-medium text-gray-700 mb-1">Device Wattage (W)</label>
                  <input 
                    type="number" 
                    placeholder="e.g. 1000" 
                    value={wattage} 
                    onChange={(e) => setWattage(e.target.value)} 
                    data-testid="wattage-input"
                    className="w-full px-4 py-2 border border-gray-300 rounded-lg focus:ring-2 focus:ring-blue-500 focus:border-blue-500 outline-none"
                  />
                </div>
                <div>
                  <label className="block text-sm font-medium text-gray-700 mb-1">Daily Usage (Hours)</label>
                  <input 
                    type="number" 
                    placeholder="e.g. 2" 
                    value={hours} 
                    onChange={(e) => setHours(e.target.value)} 
                    data-testid="hours-input"
                    className="w-full px-4 py-2 border border-gray-300 rounded-lg focus:ring-2 focus:ring-blue-500 focus:border-blue-500 outline-none"
                  />
                </div>
                <button 
                  onClick={handleCalculate} 
                  data-testid="calculate-button"
                  className="w-full flex items-center justify-center space-x-2 bg-blue-600 text-white font-semibold py-2 px-4 rounded-lg hover:bg-blue-700 transition duration-200 mt-2"
                >
                  <Calculator className="w-4 h-4" />
                  <span>Calculate Cost</span>
                </button>
              </div>

              {/* Results */}
              <div className="bg-gray-50 rounded-lg border border-gray-100 p-6 flex flex-col justify-center items-center text-center">
                <h3 className="text-sm font-medium text-gray-500 mb-2">Estimated Cost</h3>
                {cost !== null ? (
                  <div className="space-y-1">
                    <p data-testid="cost-result" className="text-4xl font-bold text-gray-900">
                      ${cost.toFixed(2)}
                    </p>
                    <p className="text-sm text-gray-500">Based on $0.15/kWh</p>
                  </div>
                ) : (
                  <p className="text-gray-400 text-sm">
                    Enter details and calculate to see the estimated cost.
                  </p>
                )}
              </div>
            </div>
          </div>
        </div>
      </div>
    </div>
  );
}

export default App;
