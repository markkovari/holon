import React, { useState } from 'react';
import { Chart as ChartJS, ArcElement, Tooltip, Legend } from 'chart.js';
import { Pie } from 'react-chartjs-2';

ChartJS.register(ArcElement, Tooltip, Legend);

function App() {
  const [balance, setBalance] = useState(0);
  
  const data = {
    labels: ['Food', 'Rent', 'Entertainment'],
    datasets: [
      {
        data: [300, 1500, 200],
        backgroundColor: ['#FF6384', '#36A2EB', '#FFCE56'],
      },
    ],
  };

  return (
    <div>
      <h1>Budget Tracker</h1>
      <p>Balance: ${balance}</p>
      <div style={{ width: '400px' }}>
        <Pie data={data} />
      </div>
    </div>
  );
}

export default App;
